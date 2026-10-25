use super::*;
use crate::l3::legacy_traffic::ServiceOptions;
fn request(number: u8) -> ServiceMessage {
    ServiceMessage::Request {
        request_number: number,
        service_code: 4,
        options: ServiceOptions::default(),
    }
}
#[test]
fn service_requires_request_ack_then_final_response_transmission_and_retries_same_number() {
    let mut endpoint = Endpoint::default();
    endpoint.receive(request(15), 0).unwrap();
    assert!(endpoint.transmission().is_none());
    assert_eq!(endpoint.acknowledge(10), Err(ServiceError::NotTransmitted));
    endpoint.request_ack_transmitted();
    let first = endpoint.transmission().unwrap().clone();
    endpoint.receive(request(15), 20).unwrap();
    assert!(first.matches(endpoint.transmission().unwrap()));
    assert_eq!(endpoint.count(), 1);
    assert!(matches!(
        first.message(),
        TrafficMessage::Service(ServiceMessage::Response {
            request_number: 0,
            service_code: 6,
            ..
        })
    ));
    endpoint.transmitted(&first, 100).unwrap();
    endpoint.advance(299).unwrap();
    assert!(endpoint.transmission().is_none());
    endpoint.advance(300).unwrap();
    let second = endpoint.transmission().unwrap().clone();
    assert!(!first.matches(&second));
    assert_eq!(first.message(), second.message());
    assert_eq!(
        endpoint.transmitted(&first, 300),
        Err(ServiceError::StaleTransmission)
    );
    endpoint.transmitted(&second, 310).unwrap();
    endpoint.acknowledge(311).unwrap();
    assert_eq!(endpoint.state(), Some(ServiceState::Acknowledged));
    endpoint.receive(request(0), 320).unwrap();
    assert_eq!(endpoint.count(), 2);
    assert_eq!(endpoint.acknowledge(321), Err(ServiceError::NotTransmitted));
    endpoint.request_ack_transmitted();
    assert!(matches!(
        endpoint.transmission().unwrap().message(),
        TrafficMessage::Service(ServiceMessage::Response {
            request_number: 1,
            ..
        })
    ));
}
#[test]
fn service_timeout_duplicate_and_release_do_not_extend_pending_exchange() {
    let mut endpoint = Endpoint::default();
    endpoint.receive(request(0), 0).unwrap();
    assert_eq!(endpoint.receive(request(1), 10), Err(ServiceError::Busy));
    let mut changed = request(0);
    if let ServiceMessage::Request { service_code, .. } = &mut changed {
        *service_code = 1
    }
    assert_eq!(
        endpoint.receive(changed, 10),
        Err(ServiceError::ChangedRetransmission)
    );
    endpoint.receive(request(0), 4999).unwrap();
    endpoint.advance(5000).unwrap();
    assert_eq!(endpoint.state(), Some(ServiceState::TimedOut));
    assert_eq!(endpoint.acknowledge(5001), Err(ServiceError::Closed));
    endpoint.receive(request(0), 5010).unwrap();
    assert_eq!(endpoint.count(), 1);
    endpoint.request_ack_transmitted();
    for now in [5020, 5220, 5420] {
        let tx = endpoint.transmission().unwrap().clone();
        endpoint.transmitted(&tx, now).unwrap();
        endpoint.advance(now + 200).unwrap();
    }
    assert_eq!(endpoint.state(), Some(ServiceState::TimedOut));
    endpoint.receive(request(1), 5700).unwrap();
    endpoint.cancel();
    endpoint.request_ack_transmitted();
    assert_eq!(endpoint.state(), Some(ServiceState::Cancelled));
    assert!(endpoint.transmission().is_none());
    assert_eq!(endpoint.advance(5699), Err(ServiceError::ClockRegression));
}
#[test]
fn service_response_sequence_wraps_independently_of_incoming_numbers() {
    let mut endpoint = Endpoint::default();
    for sequence in 0..18u8 {
        let now = u64::from(sequence) * 10;
        endpoint
            .receive(request((sequence + 5) % REQUEST_MODULUS), now)
            .unwrap();
        endpoint.request_ack_transmitted();
        let tx = endpoint.transmission().unwrap().clone();
        let TrafficMessage::Service(message) = tx.message() else {
            panic!()
        };
        assert_eq!(message.request_number(), sequence % REQUEST_MODULUS);
        endpoint.transmitted(&tx, now + 1).unwrap();
        endpoint.acknowledge(now + 2).unwrap();
    }
    assert_eq!(endpoint.count(), 18);
}

#[test]
fn service_policy_only_accepts_unchanged_full_rate_vselp_without_privacy() {
    let baseline = ServiceOptions {
        bandwidth: Some(vec![1]),
        voice_mode: Some(vec![8]),
        ..Default::default()
    };
    for code in [1, 2, 3] {
        assert_eq!(rejection(&baseline, code), None);
    }
    for code in [0, 4, 5, 6, 7, 8, 9, 15] {
        assert_eq!(
            rejection(&baseline, code),
            Some(ServiceRejection::UnsupportedService)
        );
    }
    for (options, cause) in [
        (
            ServiceOptions {
                bandwidth: Some(vec![4]),
                ..baseline.clone()
            },
            ServiceRejection::TryFullRate,
        ),
        (
            ServiceOptions {
                voice_mode: Some(vec![16]),
                ..baseline.clone()
            },
            ServiceRejection::UnsupportedCodec,
        ),
        (
            ServiceOptions {
                voice_mode: Some(vec![9]),
                ..baseline.clone()
            },
            ServiceRejection::UnsupportedPrivacy,
        ),
        (
            ServiceOptions {
                voice_mode: None,
                extended_voice_mode: Some(vec![32]),
                ..baseline.clone()
            },
            ServiceRejection::UnsupportedCodec,
        ),
        (
            ServiceOptions {
                interleaving: Some(vec![0]),
                ..baseline.clone()
            },
            ServiceRejection::UnsupportedCodec,
        ),
    ] {
        assert_eq!(rejection(&options, 1), Some(cause));
    }
    let invalid = ServiceMessage::Request {
        request_number: 0,
        service_code: 1,
        options: ServiceOptions {
            extended_voice_mode: Some(vec![32]),
            ..baseline
        },
    };
    assert!(TrafficMessage::Service(invalid).encode().is_err());
}

#[test]
fn acelp_service_requests_preserve_the_assigned_codec_and_privacy() {
    use crate::codec::speech::Codec;
    let acelp = ServiceOptions {
        voice_mode: Some(vec![16]),
        ..Default::default()
    };
    assert_eq!(rejection_for_codec(&acelp, 1, Codec::AcelpCc1), None);
    assert_eq!(
        rejection_for_codec(&acelp, 1, Codec::Vselp),
        Some(ServiceRejection::UnsupportedCodec)
    );
    let vselp = ServiceOptions {
        voice_mode: Some(vec![8]),
        ..Default::default()
    };
    assert_eq!(
        rejection_for_codec(&vselp, 1, Codec::AcelpCc1),
        Some(ServiceRejection::UnsupportedCodec)
    );
    let private = ServiceOptions {
        voice_mode: Some(vec![17]),
        ..Default::default()
    };
    assert_eq!(
        rejection_for_codec(&private, 1, Codec::AcelpCc1),
        Some(ServiceRejection::UnsupportedPrivacy)
    );
}
