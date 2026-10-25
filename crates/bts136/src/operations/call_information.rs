use super::proto;
use crate::l3::legacy_traffic::{CallInformation, CallingName, CallingNumber, MessageWaiting};
const MAX_OTHER_MESSAGES: usize = 16;
const MAX_MESSAGE_COUNT: u32 = 63;
const MAX_MESSAGE_KIND: u32 = 15;
const MAX_SIGNAL_CODE: u32 = 191;
pub(super) fn fields(
    request: &proto::SendCallInformationRequest,
) -> Result<CallInformation, tonic::Status> {
    if request.call_id == 0 {
        return Err(tonic::Status::invalid_argument("Call ID must be positive"));
    }
    proto::CallParty::try_from(request.party)
        .map_err(|_| tonic::Status::invalid_argument("Unknown call party"))?;
    let mut fields = CallInformation::default();
    if let Some(number) = &request.calling_number {
        fields.calling_numbers.push(
            CallingNumber::network(number)
                .map_err(|error| tonic::Status::invalid_argument(error.to_string()))?,
        );
    }
    if let Some(name) = &request.calling_name {
        fields.calling_name = Some(
            CallingName::network(name)
                .map_err(|error| tonic::Status::invalid_argument(error.to_string()))?,
        );
    }
    if let Some(count) = request.voice_messages {
        if count > MAX_MESSAGE_COUNT {
            return Err(tonic::Status::invalid_argument(
                "Voice message count must be 0–63",
            ));
        }
        fields.voice_messages = Some(vec![count as u8]);
    }
    if let Some(signal) = request.signal_code {
        if signal > MAX_SIGNAL_CODE {
            return Err(tonic::Status::invalid_argument(
                "Signal code contains a reserved pitch",
            ));
        }
        fields.signals = Some(vec![signal as u8]);
    }
    if request.other_messages.len() > MAX_OTHER_MESSAGES {
        return Err(tonic::Status::invalid_argument(
            "At most 16 message types may be supplied",
        ));
    }
    if !request.other_messages.is_empty() {
        let mut messages = Vec::with_capacity(request.other_messages.len());
        for message in &request.other_messages {
            if message.kind > MAX_MESSAGE_KIND || message.count > MAX_MESSAGE_COUNT {
                return Err(tonic::Status::invalid_argument(
                    "Message type must be 0–15 and count must be 0–63",
                ));
            }
            messages.push(MessageWaiting {
                kind: message.kind as u8,
                count: message.count as u8,
            });
        }
        fields.other_messages = Some(messages);
    }
    if fields.is_empty() {
        return Err(tonic::Status::invalid_argument(
            "At least one call-information field is required",
        ));
    }
    fields
        .validate()
        .map_err(|error| tonic::Status::invalid_argument(error.to_string()))?;
    Ok(fields)
}
pub(super) fn send(
    voice: &mut crate::runtime::voice::VoiceRuntime,
    request: proto::SendCallInformationRequest,
    now_ms: u64,
) -> Result<proto::CommandResult, tonic::Status> {
    let fields = fields(&request)?;
    let party = match proto::CallParty::try_from(request.party)
        .map_err(|_| tonic::Status::invalid_argument("Unknown call party"))?
    {
        proto::CallParty::Caller => crate::core::call_session::Party::Caller,
        proto::CallParty::Callee => crate::core::call_session::Party::Callee,
    };
    voice
        .send_call_information(request.call_id, party, fields, now_ms)
        .map_err(|error| tonic::Status::failed_precondition(error.to_string()))?;
    Ok(proto::CommandResult {
        message: "Call information queued. Handset acknowledgment is reported in call details"
            .into(),
        ..Default::default()
    })
}
