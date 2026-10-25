use super::*;
use std::{
    io::{Read, Write},
    net::TcpStream,
    time::Duration,
};
fn config() -> crate::config::Config {
    crate::config::Config::from_yaml(
        "radio:\n  kind: noop\ncell:\n  band: cellular800\n  channel: 991\n  sid: 42\n  dvcc: 1\n",
    )
    .unwrap()
}
fn http(address: SocketAddr, request: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(address).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    stream.write_all(request).unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    response
}
#[tokio::test]
async fn native_grpc_and_embedded_assets_share_a_listener_and_stop_with_open_watch() {
    let (hub, _interface) = Hub::new(&config());
    let server = Server::start(SocketAddr::from(([127, 0, 0, 1], 0)), hub.clone()).unwrap();
    let address = server.address();
    let mut client =
        proto::operations_client::OperationsClient::connect(format!("http://{address}"))
            .await
            .unwrap();
    let snapshot = client
        .get_snapshot(proto::Empty {})
        .await
        .unwrap()
        .into_inner();
    assert_eq!(snapshot.phase(), proto::CellPhase::Starting);
    assert!(
        snapshot
            .carrier
            .iter()
            .any(|metric| metric.name == "Channel" && metric.value.as_deref() == Some("991"))
    );
    let mut stream = client.watch(proto::Empty {}).await.unwrap().into_inner();
    assert_eq!(stream.message().await.unwrap().unwrap().revision, 0);
    let mut updated = snapshot;
    updated.revision = 7;
    updated.set_phase(proto::CellPhase::Running);
    hub.snapshots
        .send_replace(std::sync::Arc::new(crate::operations::Published {
            snapshot: updated,
            ..Default::default()
        }));
    assert_eq!(stream.message().await.unwrap().unwrap().revision, 7);
    let body = http(
        address,
        b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    assert!(String::from_utf8_lossy(&body).contains("<title>136BTS</title>"));
    let icon = http(
        address,
        b"GET /favicon.svg HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
    );
    assert!(String::from_utf8_lossy(&icon).contains("content-type: image/svg+xml"));
    let blocked = http(address,b"POST /bts136.operations.v1.Operations/SendSms HTTP/1.1\r\nHost: localhost\r\nOrigin: null\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
    assert!(String::from_utf8_lossy(&blocked).starts_with("HTTP/1.1 403"));
    server.stop().unwrap();
    assert!(TcpStream::connect(address).is_err());
}
#[test]
fn grpc_web_binary_snapshot_uses_protobuf_and_trailers() {
    use prost::Message;
    let (hub, _interface) = Hub::new(&config());
    let server = Server::start(SocketAddr::from(([127, 0, 0, 1], 0)), hub).unwrap();
    let header=b"POST /bts136.operations.v1.Operations/GetSnapshot HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/grpc-web+proto\r\nX-Grpc-Web: 1\r\nContent-Length: 5\r\nConnection: close\r\n\r\n";
    let mut request = header.to_vec();
    request.extend([0u8; 5]);
    let response = http(server.address(), &request);
    let split = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap()
        + 4;
    let headers = String::from_utf8_lossy(&response[..split]);
    assert!(headers.contains("application/grpc-web+proto"));
    let mut body = response[split..].to_vec();
    if headers
        .to_lowercase()
        .contains("transfer-encoding: chunked")
    {
        let mut position = 0;
        let mut decoded = Vec::new();
        while position < body.len() {
            let end = position
                + body[position..]
                    .windows(2)
                    .position(|window| window == b"\r\n")
                    .unwrap();
            let size =
                usize::from_str_radix(std::str::from_utf8(&body[position..end]).unwrap(), 16)
                    .unwrap();
            if size == 0 {
                break;
            }
            position = end + 2;
            decoded.extend_from_slice(&body[position..position + size]);
            position += size + 2;
        }
        body = decoded;
    }
    assert_eq!(body[0], 0);
    let length = u32::from_be_bytes(body[1..5].try_into().unwrap()) as usize;
    let snapshot = proto::Snapshot::decode(&body[5..5 + length]).unwrap();
    assert_eq!(snapshot.phase(), proto::CellPhase::Starting);
    assert_eq!(body[5 + length], 128);
    assert!(String::from_utf8_lossy(&body[10 + length..]).contains("grpc-status:0"));
    server.stop().unwrap();
}
#[tokio::test]
async fn invalid_sms_is_rejected_and_bounded_commands_report_saturation() {
    let (hub, interface) = Hub::new(&config());
    let service = Service(hub);
    let invalid = service
        .send_sms(Request::new(proto::SendSmsRequest {
            text: "é".into(),
            ..Default::default()
        }))
        .await
        .unwrap_err();
    assert_eq!(invalid.code(), tonic::Code::InvalidArgument);
    for _ in 0..super::super::COMMAND_CAPACITY {
        let (response, _receiver) = oneshot::channel();
        service
            .0
            .commands
            .try_send(PendingCommand {
                command: Command::Release(proto::ReleaseCallRequest { call_id: 1 }),
                response,
            })
            .unwrap();
    }
    let full = service
        .release_call(Request::new(proto::ReleaseCallRequest { call_id: 1 }))
        .await
        .unwrap_err();
    assert_eq!(full.code(), tonic::Code::ResourceExhausted);
    while interface.commands.try_recv().is_ok() {}
    drop(interface);
    let stopped = service
        .release_call(Request::new(proto::ReleaseCallRequest { call_id: 1 }))
        .await
        .unwrap_err();
    assert_eq!(stopped.code(), tonic::Code::Unavailable);
}

#[test]
fn browser_origin_checks_allow_only_listener_or_explicit_proxy_origin() {
    let mut headers = HeaderMap::new();
    headers.insert(header::HOST, "127.0.0.1:8080".parse().unwrap());
    headers.insert(header::ORIGIN, "http://127.0.0.1:8080".parse().unwrap());
    assert!(origin_allowed(&headers, &[]));
    headers.insert(header::ORIGIN, "null".parse().unwrap());
    assert!(!origin_allowed(&headers, &[]));
    headers.insert(header::ORIGIN, "http://localhost:9090".parse().unwrap());
    assert!(!origin_allowed(&headers, &[]));
    assert!(origin_allowed(&headers, &["http://localhost:9090".into()]));
}

#[tokio::test]
async fn native_measurement_rpc_dispatches_the_named_call_leg_through_the_radio_queue() {
    let (hub, interface) = Hub::new(&config());
    let server = Server::start(SocketAddr::from(([127, 0, 0, 1], 0)), hub).unwrap();
    let mut client =
        proto::operations_client::OperationsClient::connect(format!("http://{}", server.address()))
            .await
            .unwrap();
    let invalid = client
        .set_measurements(proto::SetMeasurementsRequest {
            call_id: 12,
            party: 99,
            enabled: true,
            extended: None,
        })
        .await
        .unwrap_err();
    assert_eq!(invalid.code(), tonic::Code::InvalidArgument);
    assert!(interface.commands.try_recv().is_err());
    let dispatch = tokio::task::spawn_blocking(move || {
        for extended in [false, true] {
            let pending = interface
                .commands
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            let Command::Measurements(request) = pending.command else {
                panic!("expected measurement command")
            };
            assert_eq!(request.call_id, 12);
            assert_eq!(request.party, proto::CallParty::Callee as i32);
            assert_eq!(request.enabled, extended);
            if extended {
                let fields = request.extended.unwrap();
                assert_eq!(
                    fields.kind,
                    proto::ExtendedMeasurementKind::ExtendedWordErrorRate as i32
                );
                assert_eq!(fields.channels.len(), 1);
                assert_eq!(fields.channels[0].channel, 991);
            } else {
                assert!(request.extended.is_none());
            }
            pending
                .response
                .send(Ok(proto::CommandResult {
                    message: "Measurement reporting requested".into(),
                    ..Default::default()
                }))
                .unwrap();
        }
    });
    let result = client
        .set_measurements(proto::SetMeasurementsRequest {
            call_id: 12,
            party: proto::CallParty::Callee as i32,
            enabled: false,
            extended: None,
        })
        .await
        .unwrap()
        .into_inner();
    assert_eq!(result.message, "Measurement reporting requested");
    let invalid = client
        .set_measurements(proto::SetMeasurementsRequest {
            call_id: 12,
            party: proto::CallParty::Callee as i32,
            enabled: false,
            extended: Some(proto::ExtendedMeasurements {
                kind: proto::ExtendedMeasurementKind::ExtendedWordErrorRate as i32,
                channels: vec![],
            }),
        })
        .await
        .unwrap_err();
    assert_eq!(invalid.code(), tonic::Code::InvalidArgument);
    client
        .set_measurements(proto::SetMeasurementsRequest {
            call_id: 12,
            party: proto::CallParty::Callee as i32,
            enabled: true,
            extended: Some(proto::ExtendedMeasurements {
                kind: proto::ExtendedMeasurementKind::ExtendedWordErrorRate as i32,
                channels: vec![proto::MeasurementChannel {
                    band: proto::MeasurementBand::MeasurementCellular800 as i32,
                    channel: 991,
                }],
            }),
        })
        .await
        .unwrap();
    dispatch.await.unwrap();
    drop(client);
    server.stop().unwrap();
}

#[tokio::test]
async fn sms_rpc_checks_the_single_relay_ira_capacity_before_dispatch() {
    let (hub, interface) = Hub::new(&config());
    let service = Service(hub);
    let request = |length| proto::SendSmsRequest {
        request_id: "maximum-sms".into(),
        sender_number: "1001".into(),
        recipient_number: "1002".into(),
        text: "x".repeat(length),
    };
    let invalid = service
        .send_sms(Request::new(request(MAX_SMS_USER_OCTETS + 1)))
        .await
        .unwrap_err();
    assert_eq!(invalid.code(), tonic::Code::InvalidArgument);
    assert!(interface.commands.try_recv().is_err());
    let dispatch = tokio::task::spawn_blocking(move || {
        let pending = interface
            .commands
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        let Command::Sms(request) = pending.command else {
            panic!()
        };
        assert_eq!(
            request.text.len(),
            crate::l3::sms::MAX_SINGLE_RELAY_IRA_CHARACTERS
        );
        pending
            .response
            .send(Ok(proto::CommandResult {
                message: "Queued for delivery".into(),
                ..Default::default()
            }))
            .unwrap();
    });
    let result = service
        .send_sms(Request::new(request(MAX_SMS_USER_OCTETS)))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(result.message, "Queued for delivery");
    dispatch.await.unwrap();
}

#[tokio::test]
async fn status_rpc_validates_call_and_party_then_dispatches_to_the_radio_queue() {
    let (hub, interface) = Hub::new(&config());
    let service = Service(hub);
    for request in [
        proto::RequestStatusRequest {
            call_id: 0,
            party: 0,
        },
        proto::RequestStatusRequest {
            call_id: 7,
            party: 99,
        },
    ] {
        assert_eq!(
            service
                .request_status(Request::new(request))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::InvalidArgument
        );
        assert!(interface.commands.try_recv().is_err());
    }
    let dispatch = tokio::task::spawn_blocking(move || {
        let pending = interface
            .commands
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        let Command::Status(request) = pending.command else {
            panic!()
        };
        assert_eq!(request.call_id, 7);
        assert_eq!(request.party, proto::CallParty::Callee as i32);
        pending
            .response
            .send(Ok(proto::CommandResult {
                message: "Phone status requested".into(),
                ..Default::default()
            }))
            .unwrap();
    });
    let result = service
        .request_status(Request::new(proto::RequestStatusRequest {
            call_id: 7,
            party: proto::CallParty::Callee as i32,
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(result.message, "Phone status requested");
    dispatch.await.unwrap();
}

#[tokio::test]
async fn native_physical_control_rpc_validates_limits_and_preserves_optional_zero_values() {
    let (hub, interface) = Hub::new(&config());
    let server = Server::start(SocketAddr::from(([127, 0, 0, 1], 0)), hub).unwrap();
    let mut client =
        proto::operations_client::OperationsClient::connect(format!("http://{}", server.address()))
            .await
            .unwrap();
    let base = proto::SetPhysicalControlRequest {
        call_id: 1,
        mobile_power_level: Some(0),
        timing_advance_half_symbols: Some(0),
        ..Default::default()
    };
    for invalid in [
        proto::SetPhysicalControlRequest { call_id: 0, ..base },
        proto::SetPhysicalControlRequest { party: 99, ..base },
        proto::SetPhysicalControlRequest {
            mobile_power_level: None,
            timing_advance_half_symbols: None,
            ..base
        },
        proto::SetPhysicalControlRequest {
            mobile_power_level: Some(11),
            ..base
        },
        proto::SetPhysicalControlRequest {
            timing_advance_half_symbols: Some(31),
            ..base
        },
        proto::SetPhysicalControlRequest {
            mobile_power_level: Some(u32::MAX),
            ..base
        },
        proto::SetPhysicalControlRequest {
            automatic_timing: Some(true),
            ..base
        },
    ] {
        assert_eq!(
            client
                .set_physical_control(invalid)
                .await
                .unwrap_err()
                .code(),
            tonic::Code::InvalidArgument
        );
        assert!(interface.commands.try_recv().is_err());
    }
    let dispatch = tokio::task::spawn_blocking(move || {
        for (power, advance, automatic) in [
            (Some(0), Some(0), None),
            (Some(10), Some(30), None),
            (None, None, Some(true)),
        ] {
            let pending = interface
                .commands
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            let Command::PhysicalControl(request) = pending.command else {
                panic!("expected physical control command");
            };
            assert_eq!(request.call_id, 1);
            assert_eq!(request.mobile_power_level, power);
            assert_eq!(request.timing_advance_half_symbols, advance);
            assert_eq!(request.automatic_timing, automatic);
            pending
                .response
                .send(Ok(proto::CommandResult {
                    message: "Physical controls requested".into(),
                    ..Default::default()
                }))
                .unwrap();
        }
    });
    client.set_physical_control(base).await.unwrap();
    client
        .set_physical_control(proto::SetPhysicalControlRequest {
            mobile_power_level: Some(10),
            timing_advance_half_symbols: Some(30),
            ..base
        })
        .await
        .unwrap();
    client
        .set_physical_control(proto::SetPhysicalControlRequest {
            call_id: 1,
            automatic_timing: Some(true),
            ..Default::default()
        })
        .await
        .unwrap();
    dispatch.await.unwrap();
    drop(client);
    server.stop().unwrap();
}

#[tokio::test]
async fn native_flash_call_information_rpc_validates_wire_bounds_and_preserves_zero_notifications()
{
    let (hub, interface) = Hub::new(&config());
    let server = Server::start(SocketAddr::from(([127, 0, 0, 1], 0)), hub).unwrap();
    let mut client =
        proto::operations_client::OperationsClient::connect(format!("http://{}", server.address()))
            .await
            .unwrap();
    let base = proto::SendCallInformationRequest {
        call_id: 1,
        party: proto::CallParty::Callee as i32,
        voice_messages: Some(0),
        signal_code: Some(0),
        calling_number: Some("1001".into()),
        calling_name: Some("Alice".into()),
        ..Default::default()
    };
    for invalid in [
        proto::SendCallInformationRequest {
            call_id: 0,
            ..base.clone()
        },
        proto::SendCallInformationRequest {
            party: 17,
            ..base.clone()
        },
        proto::SendCallInformationRequest {
            signal_code: Some(13),
            ..base.clone()
        },
        proto::SendCallInformationRequest {
            signal_code: Some(191),
            ..base.clone()
        },
        proto::SendCallInformationRequest {
            voice_messages: Some(64),
            ..base.clone()
        },
        proto::SendCallInformationRequest {
            calling_name: Some("x".repeat(61)),
            ..base.clone()
        },
        proto::SendCallInformationRequest {
            calling_number: Some("x".repeat(16)),
            ..base.clone()
        },
        proto::SendCallInformationRequest {
            other_messages: vec![proto::WaitingMessage { kind: 1, count: 0 }; 17],
            ..base.clone()
        },
        proto::SendCallInformationRequest {
            call_id: 1,
            ..Default::default()
        },
    ] {
        assert_eq!(
            client
                .send_call_information(invalid)
                .await
                .unwrap_err()
                .code(),
            tonic::Code::InvalidArgument
        );
        assert!(interface.commands.try_recv().is_err());
    }
    let expected = base.clone();
    let dispatch = tokio::task::spawn_blocking(move || {
        let pending = interface
            .commands
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        let Command::CallInformation(request) = pending.command else {
            panic!()
        };
        assert_eq!(request, expected);
        pending
            .response
            .send(Ok(proto::CommandResult {
                message: "Queued".into(),
                ..Default::default()
            }))
            .unwrap();
    });
    assert_eq!(
        client
            .send_call_information(base)
            .await
            .unwrap()
            .into_inner()
            .message,
        "Queued"
    );
    dispatch.await.unwrap();
    drop(client);
    server.stop().unwrap();
}
#[tokio::test]
async fn capability_poll_rpc_validates_identity_and_uses_bounded_command_path() {
    let (hub, interface) = Hub::new(&config());
    let server = Server::start(SocketAddr::from(([127, 0, 0, 1], 0)), hub).unwrap();
    let mut client =
        proto::operations_client::OperationsClient::connect(format!("http://{}", server.address()))
            .await
            .unwrap();
    let invalid = client
        .request_capabilities(proto::SubscriberIdentity {
            kind: 99,
            value: 1,
            esn: None,
        })
        .await
        .unwrap_err();
    assert_eq!(invalid.code(), tonic::Code::InvalidArgument);
    assert!(interface.commands.try_recv().is_err());
    let task = tokio::spawn(async move {
        client
            .request_capabilities(proto::SubscriberIdentity {
                kind: 2,
                value: 1,
                esn: None,
            })
            .await
    });
    let pending = loop {
        if let Ok(command) = interface.commands.try_recv() {
            break command;
        }
        tokio::task::yield_now().await;
    };
    let Command::Capabilities(id) = pending.command else {
        panic!()
    };
    assert_eq!((id.kind, id.value), (2, 1));
    pending
        .response
        .send(Ok(proto::CommandResult {
            message: "Capability request queued".into(),
            ..Default::default()
        }))
        .unwrap();
    assert_eq!(
        task.await.unwrap().unwrap().into_inner().message,
        "Capability request queued"
    );
    server.stop().unwrap();
}
#[tokio::test]
async fn audit_rpc_validates_identity_and_preserves_standard_order_flags() {
    let (hub, interface) = Hub::new(&config());
    let service = Service(hub);
    assert_eq!(
        service
            .audit_subscriber(Request::new(proto::AuditSubscriberRequest::default()))
            .await
            .unwrap_err()
            .code(),
        tonic::Code::InvalidArgument
    );
    let operation = tokio::spawn(async move {
        service
            .audit_subscriber(Request::new(proto::AuditSubscriberRequest {
                subscriber_id: Some(proto::SubscriberIdentity {
                    kind: 2,
                    value: 1,
                    esn: None,
                }),
                force_registration: true,
                debug_display_allowed: true,
            }))
            .await
    });
    let pending = loop {
        if let Ok(pending) = interface.commands.try_recv() {
            break pending;
        }
        tokio::task::yield_now().await;
    };
    let Command::Audit(request) = pending.command else {
        panic!()
    };
    assert!(request.force_registration && request.debug_display_allowed);
    pending
        .response
        .send(Ok(proto::CommandResult {
            message: "Audit order queued".into(),
            ..Default::default()
        }))
        .unwrap();
    assert_eq!(
        operation.await.unwrap().unwrap().into_inner().message,
        "Audit order queued"
    );
}

#[tokio::test]
async fn sms_rpc_accepts_maximum_latin1_and_bmp_text_by_encoded_size() {
    for text in ["é".repeat(248), "中".repeat(124)] {
        let (hub, interface) = Hub::new(&config());
        let service = Service(hub);
        let expected = text.clone();
        let dispatch = tokio::task::spawn_blocking(move || {
            let pending = interface
                .commands
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            let Command::Sms(request) = pending.command else {
                panic!()
            };
            assert_eq!(request.text, expected);
            pending
                .response
                .send(Ok(proto::CommandResult {
                    message: "Queued".into(),
                    ..Default::default()
                }))
                .unwrap();
        });
        service
            .send_sms(Request::new(proto::SendSmsRequest {
                request_id: "international".into(),
                sender_number: "1001".into(),
                recipient_number: "1002".into(),
                text,
            }))
            .await
            .unwrap();
        dispatch.await.unwrap();
    }
}
#[tokio::test]
async fn sms_rpc_rejects_encoded_overflow_and_supplementary_text_before_dispatch() {
    let (hub, interface) = Hub::new(&config());
    let service = Service(hub);
    for text in ["é".repeat(249), "中".repeat(125), "😀".into()] {
        let error = service
            .send_sms(Request::new(proto::SendSmsRequest {
                request_id: "invalid-international".into(),
                sender_number: "1001".into(),
                recipient_number: "1002".into(),
                text,
            }))
            .await
            .unwrap_err();
        assert_eq!(error.code(), tonic::Code::InvalidArgument);
        assert!(interface.commands.try_recv().is_err());
    }
}

#[tokio::test]
async fn message_waiting_rpc_rejects_reserved_duplicate_and_oversize_fields_before_dispatch() {
    let (hub, interface) = Hub::new(&config());
    let service = Service(hub);
    let request = |messages: Vec<proto::WaitingMessage>, display: Option<String>| {
        proto::SendMessageWaitingRequest {
            subscriber_id: Some(proto::SubscriberIdentity {
                kind: 2,
                value: 1,
                esn: None,
            }),
            messages,
            display,
        }
    };
    for invalid in [
        request(vec![], None),
        request(vec![proto::WaitingMessage { kind: 3, count: 0 }], None),
        request(vec![proto::WaitingMessage { kind: 0, count: 64 }], None),
        request(vec![proto::WaitingMessage { kind: 0, count: 1 }; 2], None),
        request(
            vec![proto::WaitingMessage { kind: 0, count: 1 }],
            Some("x".repeat(83)),
        ),
        request(
            vec![proto::WaitingMessage { kind: 0, count: 1 }],
            Some("é".into()),
        ),
        proto::SendMessageWaitingRequest {
            subscriber_id: None,
            messages: vec![proto::WaitingMessage { kind: 0, count: 1 }],
            display: None,
        },
    ] {
        assert_eq!(
            service
                .send_message_waiting(Request::new(invalid))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::InvalidArgument
        );
        assert!(interface.commands.try_recv().is_err());
    }
}
#[tokio::test]
async fn message_waiting_rpc_preserves_clear_unknown_positive_and_display_values() {
    let (hub, interface) = Hub::new(&config());
    let service = Service(hub);
    let request = proto::SendMessageWaitingRequest {
        subscriber_id: Some(proto::SubscriberIdentity {
            kind: 2,
            value: 1,
            esn: None,
        }),
        messages: vec![
            proto::WaitingMessage { kind: 0, count: 63 },
            proto::WaitingMessage { kind: 1, count: 0 },
        ],
        display: Some("D".repeat(82)),
    };
    let expected = request.clone();
    let dispatch = tokio::task::spawn_blocking(move || {
        let pending = interface
            .commands
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        let Command::MessageWaiting(actual) = pending.command else {
            panic!()
        };
        assert_eq!(actual, expected);
        pending
            .response
            .send(Ok(proto::CommandResult {
                message: "Queued".into(),
                ..Default::default()
            }))
            .unwrap();
    });
    assert_eq!(
        service
            .send_message_waiting(Request::new(request))
            .await
            .unwrap()
            .into_inner()
            .message,
        "Queued"
    );
    dispatch.await.unwrap();
}

#[tokio::test]
async fn user_alert_rpc_validates_signal_codes_identity_and_display_before_dispatch() {
    let (hub, interface) = Hub::new(&config());
    let service = Service(hub);
    let request = |pitch, cadence, duration_code, display| proto::SendUserAlertRequest {
        subscriber_id: Some(proto::SubscriberIdentity {
            kind: 2,
            value: 1,
            esn: None,
        }),
        signal: Some(proto::ControlSignal {
            pitch,
            cadence,
            duration_code,
        }),
        display,
    };
    for invalid in [
        request(3, 1, 0, None),
        request(0, 13, 0, None),
        request(0, 1, 16, None),
        request(256, 1, 0, None),
        request(0, 1, 0, Some("é".into())),
        request(0, 1, 0, Some("X".repeat(83))),
        proto::SendUserAlertRequest {
            ..Default::default()
        },
    ] {
        assert_eq!(
            service
                .send_user_alert(Request::new(invalid))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::InvalidArgument
        );
        assert!(interface.commands.try_recv().is_err());
    }
}
#[tokio::test]
async fn user_alert_rpc_preserves_omitted_standard_signal_and_explicit_one_cycle_signal() {
    for signal in [
        None,
        Some(proto::ControlSignal {
            pitch: 2,
            cadence: 12,
            duration_code: 0,
        }),
    ] {
        let (hub, interface) = Hub::new(&config());
        let service = Service(hub);
        let request = proto::SendUserAlertRequest {
            subscriber_id: Some(proto::SubscriberIdentity {
                kind: 2,
                value: 1,
                esn: None,
            }),
            signal,
            display: Some("X".repeat(82)),
        };
        let expected = request.clone();
        let dispatch = tokio::task::spawn_blocking(move || {
            let pending = interface
                .commands
                .recv_timeout(Duration::from_secs(2))
                .unwrap();
            let Command::UserAlert(actual) = pending.command else {
                panic!()
            };
            assert_eq!(actual, expected);
            pending
                .response
                .send(Ok(proto::CommandResult {
                    message: "Queued".into(),
                    ..Default::default()
                }))
                .unwrap();
        });
        assert_eq!(
            service
                .send_user_alert(Request::new(request))
                .await
                .unwrap()
                .into_inner()
                .message,
            "Queued"
        );
        dispatch.await.unwrap();
    }
}

#[tokio::test]
async fn start_call_validates_identity_and_caller_id_and_preserves_runtime_result() {
    let (hub, interface) = Hub::new(&config());
    let service = Service(hub);
    for request in [
        proto::StartCallRequest::default(),
        proto::StartCallRequest {
            subscriber_id: Some(proto::SubscriberIdentity {
                kind: 99,
                value: 1,
                esn: None,
            }),
            calling_number: "2001".into(),
        },
        proto::StartCallRequest {
            subscriber_id: Some(proto::SubscriberIdentity {
                kind: 2,
                value: 1,
                esn: None,
            }),
            calling_number: String::new(),
        },
        proto::StartCallRequest {
            subscriber_id: Some(proto::SubscriberIdentity {
                kind: 2,
                value: 1,
                esn: None,
            }),
            calling_number: "abc".into(),
        },
    ] {
        assert_eq!(
            service
                .start_call(Request::new(request))
                .await
                .unwrap_err()
                .code(),
            tonic::Code::InvalidArgument
        );
        assert!(interface.commands.try_recv().is_err());
    }
    let caller = "2001";
    let invoke = service.start_call(Request::new(proto::StartCallRequest {
        subscriber_id: Some(proto::SubscriberIdentity {
            kind: 2,
            value: 1,
            esn: None,
        }),
        calling_number: caller.into(),
    }));
    let handle = async {
        tokio::task::yield_now().await;
        let pending = interface.commands.try_recv().unwrap();
        let Command::StartCall(request) = pending.command else {
            panic!("start command required")
        };
        assert_eq!(request.calling_number, caller);
        assert_eq!(request.subscriber_id.unwrap().value, 1);
        pending
            .response
            .send(Ok(proto::CommandResult {
                call_id: Some(42),
                message: "Call paging requested".into(),
                ..Default::default()
            }))
            .unwrap();
    };
    let (result, ()) = tokio::join!(invoke, handle);
    assert_eq!(result.unwrap().into_inner().call_id, Some(42));
}

#[tokio::test]
async fn signaling_and_observed_phones_are_listed_separately_from_status_snapshots() {
    let (hub, _interface) = Hub::new(&config());
    hub.protocol.message(
        "Reverse",
        "RACH",
        crate::link::rach::MobileIdentity::Min34(7),
        &[255],
        "RX decoded",
    );
    let server = Server::start(SocketAddr::from(([127, 0, 0, 1], 0)), hub).unwrap();
    let mut client =
        proto::operations_client::OperationsClient::connect(format!("http://{}", server.address()))
            .await
            .unwrap();
    let snapshot = client
        .get_snapshot(proto::Empty {})
        .await
        .unwrap()
        .into_inner();
    assert!(snapshot.last_uplink_unix_ms.is_some());
    let observed = client
        .list_observed_phones(proto::ObservedPhoneQuery::default())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(observed.total, 1);
    assert_eq!(observed.phones[0].identity, "Min34(7)");
    let page = client
        .list_signaling(proto::SignalingQuery::default())
        .await
        .unwrap()
        .into_inner();
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].identity, "Min34(7)");
    let scoped = client
        .list_signaling(proto::SignalingQuery {
            identity: "Min34(8)".into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .into_inner();
    assert!(scoped.messages.is_empty());
    server.stop().unwrap();
}

#[tokio::test]
async fn message_pages_combine_durable_sms_history_with_live_procedure_readings() {
    use crate::core::{
        registry::ProvisionedSubscriber,
        store::{NewSms, StateStore},
    };
    use crate::l3::sms::{DisplayTime, Metadata, SmsMessage, UserData, Validity};
    use crate::link::rach::MobileIdentity;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "136bts-history-{}-{stamp}.sqlite3",
        std::process::id()
    ));
    let mut config = config();
    config.core.state_path = path.clone();
    let mut store = StateStore::open(&path).unwrap();
    store
        .reconcile(&[
            ProvisionedSubscriber {
                identity: MobileIdentity::Min34(1),
                number: "1001".into(),
            },
            ProvisionedSubscriber {
                identity: MobileIdentity::Min34(2),
                number: "1002".into(),
            },
        ])
        .unwrap();
    let message = SmsMessage::Deliver {
        metadata: Metadata {
            message_reference: 0,
            privacy: 0,
            urgency: crate::l3::sms::NORMAL_URGENCY,
            delivery_acknowledgment: false,
            manual_acknowledgment: false,
            new_message: true,
        },
        validity: Validity::Indefinite,
        display_time: DisplayTime::Invoke,
        user_data: UserData::Ira(b"worker history".to_vec()),
    };
    let id = store
        .enqueue(NewSms {
            dedup_key: "worker",
            sender: MobileIdentity::Min34(1),
            recipient: MobileIdentity::Min34(2),
            message: &message,
            created_at_unix_ms: 1,
        })
        .unwrap()
        .value();
    drop(store);
    let (hub, _runtime) = Hub::new(&config);
    hub.snapshots
        .send_replace(std::sync::Arc::new(crate::operations::Published {
            live_messages: vec![proto::Sms {
                id,
                measurements: vec![super::super::metric("Live deadline", 42, "ms")],
                ..Default::default()
            }],
            ..Default::default()
        }));
    let service = Service(hub);
    assert_eq!(
        service
            .status_snapshot(&service.0.snapshots.borrow().clone())
            .await
            .queued_sms,
        Some(1)
    );
    let output = service
        .list_messages(Request::new(proto::ListMessagesRequest {
            number: "1002".into(),
            ..Default::default()
        }))
        .await
        .unwrap()
        .into_inner();
    assert_eq!(output.total, 1);
    assert_eq!(output.messages.len(), 1);
    assert_eq!(output.messages[0].text.as_deref(), Some("worker history"));
    assert!(
        output.messages[0].measurements.iter().any(
            |reading| reading.name == "Live deadline" && reading.value.as_deref() == Some("42")
        )
    );
    std::fs::remove_file(path).unwrap();
}
