use super::{
    proto,
    snapshot::{check_subscriber_busy, subscriber_identity},
};
use crate::runtime::{sms::SmsRuntime, voice::VoiceRuntime};

pub(super) fn save(
    sms: &mut SmsRuntime,
    voice: &VoiceRuntime,
    request: proto::SaveSubscriberRequest,
    esn: u32,
    now_ms: u64,
) -> Result<proto::CommandResult, tonic::Status> {
    let requested = request.subscriber_id.map(subscriber_identity).transpose()?;
    let existing = sms
        .core()
        .devices()
        .iter()
        .find(|device| device.esn == esn)
        .and_then(|device| device.identity);
    for identity in existing.into_iter().chain(requested) {
        check_subscriber_busy(sms, voice, identity)?;
    }
    sms.core_mut()
        .save_device(esn, request.number, requested)
        .map_err(error)?;
    sms.sync_subscribers(now_ms)
        .map_err(|error| tonic::Status::internal(error.to_string()))?;
    tracing::info!(esn = format_args!("0x{esn:08X}"), "ESN subscriber saved");
    Ok(proto::CommandResult {
        message: "Subscriber saved. Its mobile identity is learned at registration".into(),
        ..Default::default()
    })
}

pub(super) fn delete(
    sms: &mut SmsRuntime,
    voice: &VoiceRuntime,
    esn: u32,
) -> Result<proto::CommandResult, tonic::Status> {
    if let Some(identity) = sms
        .core()
        .devices()
        .iter()
        .find(|device| device.esn == esn)
        .and_then(|device| device.identity)
    {
        check_subscriber_busy(sms, voice, identity)?;
    }
    sms.core_mut().delete_device(esn).map_err(error)?;
    tracing::info!(esn = format_args!("0x{esn:08X}"), "ESN subscriber removed");
    Ok(proto::CommandResult {
        message: "Subscriber removed. Message history retained".into(),
        ..Default::default()
    })
}

fn error(error: crate::core::messaging::MessagingError) -> tonic::Status {
    use crate::core::{messaging::MessagingError, registry::RegistryError};
    match error {
        MessagingError::DeviceProvisioning(message) => tonic::Status::failed_precondition(message),
        MessagingError::Registry(
            RegistryError::DuplicateNumber | RegistryError::DuplicateIdentity,
        ) => tonic::Status::already_exists(error.to_string()),
        MessagingError::Registry(RegistryError::UnknownSubscriber) => {
            tonic::Status::not_found(error.to_string())
        }
        MessagingError::Registry(_) => tonic::Status::invalid_argument(error.to_string()),
        _ => tonic::Status::internal(error.to_string()),
    }
}
