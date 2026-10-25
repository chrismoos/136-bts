use crate::l3::{
    self, broadcast::ProtocolVersion, legacy_traffic::TrafficMessage, relay::Direction,
};
pub(super) fn traffic_name(message: &TrafficMessage) -> &'static str {
    match message {
        TrafficMessage::Dtmf(message) => match message {
            l3::legacy_traffic::DtmfMessage::Burst { .. } => "Send burst DTMF",
            l3::legacy_traffic::DtmfMessage::Continuous { .. } => "Send continuous DTMF",
            l3::legacy_traffic::DtmfMessage::BurstAck { .. } => "Send burst DTMF acknowledgment",
            l3::legacy_traffic::DtmfMessage::ContinuousAck { .. } => {
                "Send continuous DTMF acknowledgment"
            }
        },
        TrafficMessage::Flash(message) => {
            if matches!(message, l3::legacy_traffic::FlashMessage::Ack(_)) {
                "Flash with information acknowledgment"
            } else {
                "Flash with information"
            }
        }
        TrafficMessage::Service(l3::legacy_traffic::ServiceMessage::Request { .. }) => {
            "Service request"
        }
        TrafficMessage::Service(l3::legacy_traffic::ServiceMessage::Response { .. }) => {
            "Service response"
        }
        TrafficMessage::Connect => "Connect",
        TrafficMessage::Relay(relay) => match relay.message {
            l3::relay::RelayMessage::Data { .. } => "R-DATA",
            l3::relay::RelayMessage::Accept { .. } => "R-ACCEPT",
            l3::relay::RelayMessage::Reject { .. } => "R-REJECT",
        },
        TrafficMessage::MeasurementOrder { .. } => "Measurement order",
        TrafficMessage::MeasurementOrderAck { .. } => "Measurement order acknowledgment",
        TrafficMessage::StopMeasurements => "Stop measurements",
        TrafficMessage::AmrControl(message) => match message {
            l3::legacy_traffic::AmrControlMessage::Configuration(_) => "AMR configuration order",
            l3::legacy_traffic::AmrControlMessage::Thresholds(_) => "AMR threshold order",
            l3::legacy_traffic::AmrControlMessage::PhaseFreeze { .. } => {
                "AMR phase change and freeze order"
            }
        },
        TrafficMessage::AmrNoise(l3::legacy_traffic::AmrNoiseMessage::Implicit { .. }) => {
            "AMR implicit comfort noise"
        }
        TrafficMessage::AmrNoise(l3::legacy_traffic::AmrNoiseMessage::Parameters {
            command,
            ..
        }) => match command {
            l3::legacy_traffic::ActiveAmrMode::Mode1 => "AMR1 comfort noise parameters",
            l3::legacy_traffic::ActiveAmrMode::Mode2 => "AMR2 comfort noise parameters",
            l3::legacy_traffic::ActiveAmrMode::Mode3 => "AMR3 comfort noise parameters",
            l3::legacy_traffic::ActiveAmrMode::Mode4 => "AMR4 comfort noise parameters",
        },
        TrafficMessage::ComfortNoise(_) => "Comfort noise parameters",
        TrafficMessage::ChannelQuality(_) => "Channel quality report",
        TrafficMessage::ExtendedChannelQuality(_) => "Extended channel quality report",
        TrafficMessage::ExtendedMeasurementOrder(_) => "Extended measurement order",
        TrafficMessage::ExtendedMeasurementAck(_) => "Extended measurement order acknowledgment",
        TrafficMessage::Audit => "Audit",
        TrafficMessage::PhysicalControl(_) => "Physical layer control",
        TrafficMessage::PhysicalControlAck(_) => "Physical layer control acknowledgment",
        TrafficMessage::CapabilityUpdateRequest(_) => "Capability update request",
        TrafficMessage::CapabilityUpdateResponse(_) => "Capability update response",
        TrafficMessage::StatusRequest(_) => "Status request",
        TrafficMessage::Status(_) => "Status",
        TrafficMessage::AlertWithInfo { .. } => "Alert with information",
        TrafficMessage::Release { .. } => "Release",
        TrafficMessage::Ack { .. } => "Acknowledgment",
    }
}
pub(super) fn traffic_details(direction: &str, message: &TrafficMessage) -> String {
    if let TrafficMessage::ExtendedMeasurementOrder(order) = message {
        return format!(
            "{}, Mobile Ack support={:?}",
            extended_targets(&order.targets),
            order.mobile_ack_support
        );
    }
    if let TrafficMessage::ExtendedMeasurementAck(targets) = message {
        return format!("Echoed {}", extended_targets(targets));
    }
    if let TrafficMessage::ExtendedChannelQuality(report) = message {
        let (start, neighbors) = report.neighbors();
        let current = match report {
            l3::legacy_traffic::ExtendedChannelQualityReport::First { current, .. } => format!(
                "Current BER code {}, C/I or WER code {} (requires order context), RSSI code {}. ",
                current.ber_code, current.measurement_code, current.rssi_code
            ),
            _ => String::new(),
        };
        return format!(
            "{current}Neighbor positions {}–{}: {neighbors:?}",
            start + 1,
            start + neighbors.len()
        );
    }
    if let TrafficMessage::Release { information } = message {
        return super::termination::release(information);
    }
    if let TrafficMessage::Ack {
        ack_message_type,
        information,
    } = message
    {
        return super::termination::acknowledgment(*ack_message_type, information);
    }

    if let TrafficMessage::Dtmf(request) = message {
        return match request {
            l3::legacy_traffic::DtmfMessage::Burst {
                request_number,
                digits,
            } => format!(
                "Request {request_number}, digits: {}",
                digits
                    .as_ref()
                    .map(|digits| digits
                        .iter()
                        .map(|digit| digit.symbol())
                        .collect::<String>())
                    .unwrap_or_else(|| "absent".into())
            ),
            l3::legacy_traffic::DtmfMessage::Continuous {
                request_number,
                digit,
            } => format!("Request {request_number}, digit: {}", digit.symbol()),
            _ => format!("{request:?}"),
        };
    }
    if let TrafficMessage::AlertWithInfo { information } = message {
        return super::alert::details(information);
    }
    if let TrafficMessage::CapabilityUpdateResponse(response) = message {
        return super::capabilities::details(response);
    }
    if let TrafficMessage::Relay(relay) = message {
        let wire_direction = if direction == "Forward" {
            Direction::Forward
        } else {
            Direction::Reverse
        };
        let mut details = match l3::relay::addressing::decode_fields(
            relay.message.clone(),
            &relay.optional_octets,
        ) {
            Ok(decoded) => format!("{decoded:#?}"),
            Err(_) => format!("{message:#?}"),
        };
        append_sms(&mut details, &relay.message, wire_direction);
        return details;
    }
    format!("{message:#?}")
}
fn extended_targets(targets: &l3::legacy_traffic::ExtendedMeasurementTargets) -> String {
    let channels = targets
        .channels()
        .iter()
        .map(|value| format!("{:?}/{}", value.hyperband, value.channel))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "Measurement={:?}, channels=[{channels}]",
        targets.measurement_type()
    )
}
#[cfg(test)]
pub(super) fn broadcast(bytes: &[u8]) -> (String, String) {
    broadcast_in_version(bytes, Some(ProtocolVersion::Is136RevA))
}
pub(super) fn broadcast_in_version(
    bytes: &[u8],
    version: Option<ProtocolVersion>,
) -> (String, String) {
    if let Ok(message) = l3::maca::broadcast::MacaBroadcast::decode(bytes) {
        return (
            super::maca_broadcast::name(&message).into(),
            super::maca_broadcast::details(&message),
        );
    }
    if let Ok(prefix) = l3::vendor_delivery::Prefix::decode(bytes, Direction::Forward) {
        return (
            super::vendor_delivery::name(&prefix).into(),
            super::vendor_delivery::details(&prefix),
        );
    }
    if let Ok(message) = l3::operator_identity::OperatorIdentity::decode(bytes) {
        return (
            "SOC/BSMC Identification".into(),
            format!("SOC={}\nBSMC={}", message.soc, message.bsmc),
        );
    }
    if let Ok(message) = l3::time_date::TimeAndDate::decode(bytes) {
        return (
            "Time and Date".into(),
            format!(
                "SecondsSince1980={}\nUTCOffsetMinutes={}\nDaylightSaving={}",
                message.seconds_since_1980,
                message.time_zone.minutes(),
                message.time_zone.daylight_saving()
            ),
        );
    }
    if let Ok(neighbors) = l3::neighbors::decode(bytes) {
        return ("Neighbor cell".into(), format!("{neighbors:#?}"));
    }
    if let Ok(message) = l3::extended_broadcast::decode(bytes) {
        let name = match message {
            l3::extended_broadcast::ExtendedBroadcast::NeighborCell { .. } => "Neighbor cell",
            l3::extended_broadcast::ExtendedBroadcast::StandardRegulatoryConfiguration => {
                "Regulatory configuration"
            }
        };
        return (name.into(), format!("{message:#?}"));
    }
    if let Ok(decoded) = l3::broadcast::decode_prefix(bytes) {
        return (
            format!("{:?}", decoded.message)
                .split([' ', '{', '('])
                .next()
                .unwrap_or("Broadcast")
                .into(),
            format!("{:#?}", decoded.message),
        );
    }
    if bytes.first() == Some(&l3::service_menu::MESSAGE_TYPE) {
        let Some(version) = version else {
            return (
                "Service menu".into(),
                "System Identity protocol version has not been received".into(),
            );
        };
        if let Ok(decoded) = l3::service_menu::decode_prefix(bytes, version) {
            return ("Service menu".into(), format!("{:#?}", decoded.message));
        }
    }
    (
        "Unknown broadcast".into(),
        "No supported broadcast decoder matched".into(),
    )
}
pub(super) fn control(direction: &str, bytes: &[u8]) -> (String, String) {
    let direction = if direction == "Forward" {
        Direction::Forward
    } else {
        Direction::Reverse
    };
    if direction == Direction::Reverse
        && let Ok(report) = l3::maca::MacaReport::decode(bytes)
    {
        return ("MACA Report".into(), super::maca::details(&report));
    }
    if let Ok(message) = l3::call_queue::QueueMessage::decode(bytes, direction) {
        return (
            super::call_queue::name(&message).into(),
            super::call_queue::details(&message),
        );
    }
    if let Ok(message) = l3::security::SecurityMessage::decode(bytes, direction) {
        return (
            super::security::name(&message).into(),
            super::security::details(&message),
        );
    }
    if let Ok(prefix) = l3::vendor_delivery::Prefix::decode(bytes, direction) {
        return (
            super::vendor_delivery::name(&prefix).into(),
            super::vendor_delivery::details(&prefix),
        );
    }
    if direction == Direction::Reverse
        && let Ok(message) = l3::test_registration::Request::decode(bytes)
    {
        return (
            "Test Registration".into(),
            super::test_registration::request(&message),
        );
    }
    if direction == Direction::Forward
        && bytes.first() == Some(&l3::user_alert::MESSAGE_TYPE)
        && let Ok(message) = l3::user_alert::UserAlert::decode(bytes)
    {
        return ("User Alert".into(), super::user_alert::details(&message));
    }
    if direction == Direction::Forward
        && bytes.first() == Some(&l3::message_waiting::MESSAGE_TYPE)
        && let Ok(message) = l3::message_waiting::MessageWaiting::decode(bytes)
    {
        return (
            "Message Waiting".into(),
            super::message_waiting::details(&message),
        );
    }

    if direction == Direction::Reverse
        && bytes.first() == Some(&l3::notification::CONFIRMATION_TYPE)
        && let Ok(confirmation) = l3::notification::SpachConfirmation::decode(bytes)
        && matches!(
            confirmation.confirmed_type,
            l3::message_waiting::MESSAGE_TYPE | l3::user_alert::MESSAGE_TYPE
        )
    {
        return (
            "SPACH Confirmation".into(),
            super::message_waiting::confirmation(&confirmation),
        );
    }
    if let Ok(decoded) = l3::relay::addressing::decode(bytes, direction) {
        let message = &decoded.relay;
        let name = match message.message {
            l3::relay::RelayMessage::Data { .. } => "R-DATA",
            l3::relay::RelayMessage::Accept { .. } => "R-ACCEPT",
            l3::relay::RelayMessage::Reject { .. } => "R-REJECT",
        };
        let mut details = format!("{decoded:#?}");
        append_sms(&mut details, &message.message, direction);
        return (name.into(), details);
    }
    if let Ok(decoded) = l3::relay::decode_prefix(bytes, direction) {
        let name = match decoded.message {
            l3::relay::RelayMessage::Data { .. } => "R-DATA",
            l3::relay::RelayMessage::Accept { .. } => "R-ACCEPT",
            l3::relay::RelayMessage::Reject { .. } => "R-REJECT",
        };
        return (name.into(), format!("{decoded:#?}"));
    }
    if direction == Direction::Forward
        && let Ok(order) = l3::audit::AuditOrder::decode(bytes)
    {
        return ("Audit order".into(), super::audit::order(&order));
    }
    if direction == Direction::Reverse
        && let Ok(confirmation) = l3::audit::AuditConfirmation::decode(bytes)
    {
        return (
            "Audit confirmation".into(),
            super::audit::confirmation(&confirmation),
        );
    }
    if direction == Direction::Forward && bytes == l3::capability_report::request() {
        return (
            "Capability request".into(),
            "Request handset capabilities".into(),
        );
    }
    if direction == Direction::Reverse {
        if let Ok(report) = l3::capability_report::decode(bytes) {
            return (
                "Capability report".into(),
                super::capability_report::details(&report),
            );
        }
        if let Ok(decoded) = l3::registration::RegistrationRequest::decode(bytes) {
            return (
                "Registration".into(),
                super::registration::request(&decoded),
            );
        }
        if let Ok(decoded) = l3::registration::SerialNumber::decode(bytes) {
            return (
                "Serial number".into(),
                super::registration::serial(&decoded),
            );
        }
        if let Ok(decoded) = l3::registration::decode_prefix(bytes) {
            return (
                match decoded.message {
                    l3::registration::AccessMessage::Registration { .. } => "Registration",
                    l3::registration::AccessMessage::SerialNumber { .. } => "Serial number",
                }
                .into(),
                format!("{:#?}", decoded.message),
            );
        }
        if let Ok(decoded) = l3::access_call::OriginationRequest::decode(bytes) {
            return (
                "Origination".into(),
                super::access_call::origination(&decoded),
            );
        }
        if let Ok(decoded) = l3::access_call::PageResponseRequest::decode(bytes) {
            return ("Page response".into(), super::access_call::page(&decoded));
        }
        if let Ok(decoded) = l3::origination::decode_prefix(bytes) {
            return ("Origination".into(), format!("{:#?}", decoded.message));
        }
        if let Ok(decoded) = l3::page_response::decode_prefix(bytes) {
            return ("Page response".into(), format!("{:#?}", decoded.message));
        }
        if l3::notification::sms_confirmation(bytes).is_ok() {
            return (
                "SMS notification confirmation".into(),
                "SMS notification confirmed".into(),
            );
        }
    } else {
        if let Ok(message) = l3::test_registration::Response::decode(bytes) {
            return (
                "Test Registration Response".into(),
                super::test_registration::response(&message),
            );
        }
        if let Ok(decoded) = l3::registration::AcceptOptions::decode(bytes) {
            return (
                "Registration response".into(),
                super::registration::accept(&decoded),
            );
        }
        if let Ok(decoded) = l3::designation::decode_prefix(bytes) {
            return (
                "Traffic channel designation".into(),
                format!("{:#?}", decoded.message),
            );
        }
        if let Ok(response) = l3::call_control::decode_response(bytes) {
            let name = match response.kind {
                l3::call_control::ResponseKind::Release => "Release",
                l3::call_control::ResponseKind::RegistrationReject => "Registration response",
                l3::call_control::ResponseKind::Reorder => "Reorder",
            };
            return (name.into(), super::call_control::details(&response));
        }
        if let Ok(message) = l3::control_management::ControlManagement::decode(bytes) {
            let name = match &message {
                l3::control_management::ControlManagement::DirectedRetry(_) => "Directed Retry",
                l3::control_management::ControlManagement::ParameterUpdate(_) => "Parameter Update",
            };
            return (name.into(), super::control_management::details(&message));
        }
        if let Ok(page) = l3::page::Page::decode(bytes) {
            return ("Page".into(), super::page::details(&page));
        }
        if bytes == l3::notification::sms_notification() {
            return ("SMS notification".into(), "SMS delivery pending".into());
        }
    }
    (
        format!(
            "Unknown message {}",
            bytes
                .first()
                .map(|value| format!("0x{value:02X}"))
                .unwrap_or_default()
        ),
        "No supported L3 decoder matched. Raw bytes are retained.".into(),
    )
}

fn append_sms(details: &mut String, message: &l3::relay::RelayMessage, direction: Direction) {
    if let l3::relay::RelayMessage::Data {
        protocol: l3::relay::HigherLayerProtocol::Sms,
        payload,
        ..
    } = message
        && let Ok(sms) = l3::sms::decode_prefix(payload, direction)
    {
        details.push_str(&format!("\nSMS payload:\n{:#?}", sms.message));
        let data = match &sms.message {
            l3::sms::SmsMessage::Submit { user_data, .. }
            | l3::sms::SmsMessage::Deliver { user_data, .. }
            | l3::sms::SmsMessage::DeliveryAck { user_data, .. }
            | l3::sms::SmsMessage::ManualAck { user_data, .. } => user_data,
        };
        details.push_str(&format!("\nEncoding: {}", data.encoding_name()));
        if let Some(text) = data.text() {
            details.push_str(&format!("\nText: {text:?}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use l3::{
        legacy_traffic::TrafficRelay,
        relay::{HigherLayerProtocol, RelayMessage},
        sms::{Metadata, SmsMessage, UserData},
    };
    #[test]
    fn extended_measurement_logs_keep_type_channel_order_and_ack_support() {
        use l3::legacy_traffic::{
            ExtendedMeasurementOrder, ExtendedMeasurementTargets, ExtendedMeasurementType,
            Hyperband, MeasurementChannel,
        };
        let targets = ExtendedMeasurementTargets::try_new(
            ExtendedMeasurementType::WordErrorRate,
            vec![
                MeasurementChannel {
                    hyperband: Hyperband::Pcs1900,
                    channel: 42,
                },
                MeasurementChannel {
                    hyperband: Hyperband::Cellular800,
                    channel: 991,
                },
            ],
        )
        .unwrap();
        let ack = TrafficMessage::ExtendedMeasurementAck(targets.clone());
        assert_eq!(
            traffic_name(&ack),
            "Extended measurement order acknowledgment"
        );
        assert_eq!(
            traffic_details("Reverse", &ack),
            "Echoed Measurement=WordErrorRate, channels=[Pcs1900/42, Cellular800/991]"
        );
        let order = TrafficMessage::ExtendedMeasurementOrder(ExtendedMeasurementOrder {
            targets,
            mobile_ack_support: Some(false),
        });
        assert_eq!(traffic_name(&order), "Extended measurement order");
        assert_eq!(
            traffic_details("Forward", &order),
            "Measurement=WordErrorRate, channels=[Pcs1900/42, Cellular800/991], Mobile Ack support=Some(false)"
        );
    }
    #[test]
    fn extended_quality_logs_preserve_unresolved_metric_and_order_positions() {
        use l3::legacy_traffic::{
            ExtendedChannelQualityReport, ExtendedCurrentQuality, NeighborGroup,
        };
        let first = TrafficMessage::ExtendedChannelQuality(ExtendedChannelQualityReport::First {
            current: ExtendedCurrentQuality {
                ber_code: 3,
                measurement_code: 12,
                rssi_code: 42,
            },
            neighbor_rssi_codes: [0, 1, 32, 63],
        });
        assert_eq!(traffic_name(&first), "Extended channel quality report");
        let details = traffic_details("Reverse", &first);
        assert!(details.contains("C/I or WER code 12 (requires order context)"));
        assert!(details.contains("Neighbor positions 1–4: [0, 1, 32, 63]"));
        let fourth = TrafficMessage::ExtendedChannelQuality(ExtendedChannelQualityReport::Later {
            group: NeighborGroup::Fourth,
            neighbor_rssi_codes: [0; 6],
        });
        assert_eq!(
            traffic_details("Reverse", &fourth),
            "Neighbor positions 17–22: [0, 0, 0, 0, 0, 0]"
        );
    }
    #[test]
    fn traffic_sms_log_includes_text_and_retains_non_sms_protocol_identity() {
        let payload = SmsMessage::Submit {
            metadata: Metadata {
                message_reference: 42,
                privacy: 0,
                urgency: 0,
                delivery_acknowledgment: false,
                manual_acknowledgment: false,
                new_message: true,
            },
            user_data: UserData::Ira(b"hello from traffic".to_vec()),
        }
        .encode()
        .unwrap();
        let mut relay = TrafficRelay {
            message: RelayMessage::Data {
                transaction_id: 7,
                protocol: HigherLayerProtocol::Sms,
                payload,
            },
            optional_octets: vec![],
        };
        let message = TrafficMessage::Relay(relay.clone());
        assert_eq!(traffic_name(&message), "R-DATA");
        let details = traffic_details("Reverse", &message);
        assert!(details.contains("Text: \"hello from traffic\""));
        for (identity, expected) in [
            (HigherLayerProtocol::NetworkSpecific, "NetworkSpecific"),
            (
                HigherLayerProtocol::Segmented(l3::relay::Teleservice::Sms),
                "Segmented",
            ),
            (HigherLayerProtocol::Ghost, "Ghost"),
        ] {
            if let RelayMessage::Data { protocol, .. } = &mut relay.message {
                *protocol = identity;
            }
            let details = traffic_details("Reverse", &TrafficMessage::Relay(relay.clone()));
            assert!(details.contains(expected));
            assert!(!details.contains("SMS payload:"));
            assert!(!details.contains("Text:"));
        }
    }
    #[test]
    fn long_traffic_sms_logs_text_without_converting_to_dcch() {
        let text = "a".repeat(249);
        let payload = SmsMessage::Submit {
            metadata: Metadata {
                message_reference: 1,
                privacy: 0,
                urgency: 0,
                delivery_acknowledgment: false,
                manual_acknowledgment: false,
                new_message: true,
            },
            user_data: UserData::Ira(text.as_bytes().to_vec()),
        }
        .encode()
        .unwrap();
        assert_eq!(payload.len(), 254);
        let relay = TrafficRelay {
            message: RelayMessage::Data {
                transaction_id: 7,
                protocol: HigherLayerProtocol::Sms,
                payload,
            },
            optional_octets: vec![],
        };
        assert!(relay.control_message(Direction::Reverse).is_err());
        let message = TrafficMessage::Relay(relay);
        assert!(message.encode().is_ok());
        assert!(traffic_details("Reverse", &message).contains(&format!("Text: {text:?}")));
    }
}

#[cfg(test)]
mod call_response_tests {
    use super::*;
    #[test]
    fn release_and_reorder_show_new_admission_causes_in_signaling() {
        use crate::l3::registration::RejectCause;
        for cause in [
            RejectCause::UnknownMsid,
            RejectCause::Congestion,
            RejectCause::UserGroupBarred,
            RejectCause::ServiceCodeNotSupported,
            RejectCause::ServiceCodeNotAvailable,
            RejectCause::AddressingNotSupported,
            RejectCause::EncryptionModeTemporarilyDenied,
            RejectCause::VoiceCodecNotAvailable,
        ] {
            for release in [false, true] {
                let bytes = if release {
                    l3::call_control::release(cause)
                } else {
                    l3::call_control::reorder(cause)
                };
                let (name, text) = control("Forward", &bytes);
                assert_eq!(name, if release { "Release" } else { "Reorder" });
                assert!(text.contains(&format!("{cause:?}")));
            }
        }
    }
    #[test]
    fn service_menu_logs_require_actual_system_identity_version() {
        let bytes = [0x08, 0xa5, 0x86, 0x06, 0xf3, 0x6a, 0xab, 0xaa, 0x80];
        let (name, missing) = broadcast_in_version(&bytes, None);
        assert_eq!(name, "Service menu");
        assert!(missing.contains("has not been received"));
        let (_, decoded) = broadcast_in_version(&bytes, Some(ProtocolVersion::Pv2));
        assert!(decoded.contains("pv1: Some"));
        assert!(decoded.contains("sms_broadcast: Some"));
        assert!(decoded.contains("true"));
    }
    #[test]
    fn comfort_noise_logs_keep_received_codec_indices() {
        let message = TrafficMessage::ComfortNoise(l3::legacy_traffic::ComfortNoiseParameters {
            lsf_residual_vector: 0x123456,
            energy_quantization_index: 154,
            resc_parameters: 11,
        });
        assert_eq!(traffic_name(&message), "Comfort noise parameters");
        let text = traffic_details("Reverse", &message);
        assert!(text.contains("lsf_residual_vector: 1193046"));
        assert!(text.contains("energy_quantization_index: 154"));
        assert!(text.contains("resc_parameters: 11"));
    }
    #[test]
    fn amr_noise_logs_preserve_active_positions_and_unresolved_implicit_data() {
        use l3::legacy_traffic::{ActiveAmrMode, AmrNoiseMessage, AmrNoiseParameters};
        for (command, expected) in [
            (ActiveAmrMode::Mode1, "AMR1 comfort noise parameters"),
            (ActiveAmrMode::Mode2, "AMR2 comfort noise parameters"),
            (ActiveAmrMode::Mode3, "AMR3 comfort noise parameters"),
            (ActiveAmrMode::Mode4, "AMR4 comfort noise parameters"),
        ] {
            let message = TrafficMessage::AmrNoise(AmrNoiseMessage::Parameters {
                command,
                indication: ActiveAmrMode::Mode3,
                parameters: AmrNoiseParameters {
                    lsf_reference_vector: 5,
                    lsf_subvectors: [2, 3, 4],
                    frame_energy: 6,
                },
            });
            assert_eq!(traffic_name(&message), expected);
            assert!(traffic_details("Forward", &message).contains("indication: Mode3"));
        }
        let implicit = TrafficMessage::AmrNoise(AmrNoiseMessage::Implicit {
            data: ActiveAmrMode::Mode2,
        });
        assert_eq!(traffic_name(&implicit), "AMR implicit comfort noise");
        assert!(traffic_details("Reverse", &implicit).contains("data: Mode2"));
    }
    #[test]
    fn amr_control_logs_retain_configuration_thresholds_and_inactive_mode_fields() {
        use l3::legacy_traffic::{
            ActiveAmrMode, AmrActiveSet, AmrConfiguration, AmrControlMessage, AmrFreeze, AmrLink,
            AmrPhase, AmrThresholds,
        };
        let configuration =
            TrafficMessage::AmrControl(AmrControlMessage::Configuration(AmrConfiguration {
                countdown: true,
                link: AmrLink::Uplink,
                initial_mode: ActiveAmrMode::Mode2,
                active_set: AmrActiveSet::try_new(0x33).unwrap(),
                thresholds: [12, 23, 34],
                common_hysteresis: 5,
            }));
        assert_eq!(traffic_name(&configuration), "AMR configuration order");
        assert!(traffic_details("Forward", &configuration).contains("link: Uplink"));
        let thresholds = TrafficMessage::AmrControl(AmrControlMessage::Thresholds(AmrThresholds {
            lower: [4, 17, 50],
            hysteresis: [3, 9, 12],
        }));
        assert_eq!(traffic_name(&thresholds), "AMR threshold order");
        let phase = TrafficMessage::AmrControl(AmrControlMessage::PhaseFreeze {
            phase: AmrPhase::Maintain,
            freeze: AmrFreeze::Unfreeze,
            mode: ActiveAmrMode::Mode4,
        });
        assert_eq!(traffic_name(&phase), "AMR phase change and freeze order");
        let text = traffic_details("Forward", &phase);
        assert!(text.contains("freeze: Unfreeze"));
        assert!(text.contains("mode: Mode4"));
    }
}
