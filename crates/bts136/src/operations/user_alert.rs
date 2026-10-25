use super::{proto, snapshot::subscriber_identity};
use crate::{
    l3::user_alert::{Pitch, Signal, UserAlert},
    link::rach::MobileIdentity,
    runtime::{
        idle_queries::{IdleQueries, PollError},
        sms::SmsRuntime,
        voice::VoiceRuntime,
    },
};
pub(super) fn fields(
    request: &proto::SendUserAlertRequest,
) -> Result<(MobileIdentity, UserAlert), tonic::Status> {
    let identity = subscriber_identity(
        request
            .subscriber_id
            .ok_or_else(|| tonic::Status::invalid_argument("Subscriber identity is required"))?,
    )?;
    let signal = request
        .signal
        .as_ref()
        .map(|value| {
            let pitch = u8::try_from(value.pitch)
                .map_err(|_| tonic::Status::invalid_argument("Pitch is out of range"))?;
            Ok::<Signal, tonic::Status>(Signal {
                pitch: Pitch::try_from(pitch)
                    .map_err(|error| tonic::Status::invalid_argument(error.to_string()))?,
                cadence: u8::try_from(value.cadence)
                    .map_err(|_| tonic::Status::invalid_argument("Cadence is out of range"))?,
                duration_code: u8::try_from(value.duration_code).map_err(|_| {
                    tonic::Status::invalid_argument("Duration code is out of range")
                })?,
            })
        })
        .transpose()?;
    let alert = UserAlert {
        signal,
        display: request.display.clone(),
        ..Default::default()
    };
    alert
        .encode()
        .map_err(|error| tonic::Status::invalid_argument(error.to_string()))?;
    Ok((identity, alert))
}
pub(super) fn send(
    idle: &mut IdleQueries,
    sms: &SmsRuntime,
    voice: &VoiceRuntime,
    request: proto::SendUserAlertRequest,
    now_ms: u64,
) -> Result<proto::CommandResult, tonic::Status> {
    let (identity, alert) = fields(&request)?;
    idle.request_user_alert(
        sms.core().registry(),
        identity,
        alert,
        voice.subscriber_busy(identity) || sms.subscriber_busy(identity),
        now_ms,
    )
    .map_err(|error| match error {
        PollError::UnknownSubscriber => tonic::Status::not_found(error.to_string()),
        PollError::Full => tonic::Status::resource_exhausted(error.to_string()),
        _ => tonic::Status::failed_precondition(error.to_string()),
    })?;
    Ok(proto::CommandResult {
        message: "User Alert queued. Handset confirmation is reported in subscriber details".into(),
        ..Default::default()
    })
}
