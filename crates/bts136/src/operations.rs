mod device_subscribers;
pub mod proto {
    tonic::include_proto!("bts136.operations.v1");
}
mod call_information;
mod extended_measurements;
mod server;
mod snapshot;
mod subscriber_capabilities;
mod user_alert;
pub(crate) use snapshot::RuntimeStatus;
mod events;
pub(crate) mod protocol;
pub use events::EventLayer;
use proto::{CommandResult, ReleaseCallRequest, SendSmsRequest, Snapshot};
pub use server::Server;
use std::sync::{Arc, mpsc};
use tokio::sync::{oneshot, watch};
const COMMAND_CAPACITY: usize = 32;
const MAX_HISTORY_READERS: usize = 4;
pub(crate) const SNAPSHOT_INTERVAL_MS: u64 = 1000;
pub(crate) enum Command {
    Sms(SendSmsRequest),
    Release(ReleaseCallRequest),
    StartCall(proto::StartCallRequest),
    Measurements(proto::SetMeasurementsRequest),
    Status(proto::RequestStatusRequest),
    Capabilities(proto::SubscriberIdentity),
    Audit(proto::AuditSubscriberRequest),
    MessageWaiting(proto::SendMessageWaitingRequest),
    UserAlert(proto::SendUserAlertRequest),
    CallInformation(proto::SendCallInformationRequest),
    PhysicalControl(proto::SetPhysicalControlRequest),
    SaveSubscriber(proto::SaveSubscriberRequest),
    DeleteSubscriber(proto::SubscriberIdentity),
}
pub(crate) struct PendingCommand {
    pub command: Command,
    pub response: oneshot::Sender<Result<CommandResult, tonic::Status>>,
}
/// Runtime state behind the paged list RPCs. Only `snapshot` is sent by Watch.
#[derive(Clone, Default)]
pub(crate) struct Published {
    pub snapshot: Snapshot,
    pub subscribers: Vec<proto::Subscriber>,
    pub recent_calls: Vec<proto::Call>,
    pub live_messages: Vec<proto::Sms>,
}
#[cfg(test)]
pub(crate) struct TestSnapshot {
    pub subscribers: Vec<proto::Subscriber>,
    pub calls: Vec<proto::Call>,
    pub recent_calls: Vec<proto::Call>,
    pub phase: proto::CellPhase,
    pub radio: Vec<proto::Metric>,
    pub air_messages: Vec<proto::WireMessage>,
    pub observed_phones: Vec<proto::ObservedPhone>,
}
#[derive(Clone)]
pub struct Hub {
    pub(crate) snapshots: watch::Sender<Arc<Published>>,
    pub(crate) commands: mpsc::SyncSender<PendingCommand>,
    pub(crate) events: Arc<events::Events>,
    pub(crate) protocol: Arc<protocol::ProtocolLog>,
    pub(crate) state_path: std::path::PathBuf,
    pub(crate) reads: Arc<tokio::sync::Semaphore>,
}
pub struct RuntimeInterface {
    pub(crate) sms_display_time: crate::l3::sms::DisplayTime,
    pub(crate) hub: Hub,
    pub(crate) commands: mpsc::Receiver<PendingCommand>,
    pub(crate) maca_readings:
        std::collections::HashMap<crate::link::rach::MobileIdentity, Vec<proto::Metric>>,
    pub(crate) access_readings:
        std::collections::HashMap<crate::link::rach::MobileIdentity, Vec<proto::Metric>>,
    pub(crate) radio_history: snapshot::RadioHistory,
}
impl Hub {
    pub fn new(config: &crate::config::Config) -> (Self, RuntimeInterface) {
        let (snapshots, _) = watch::channel(Arc::new(Published {
            snapshot: snapshot::initial(config),
            ..Published::default()
        }));
        let (commands, receiver) = mpsc::sync_channel(COMMAND_CAPACITY);
        let hub = Self {
            snapshots,
            commands,
            events: Arc::new(events::Events::default()),
            protocol: Arc::new(protocol::ProtocolLog::default()),
            state_path: config.core.state_path.clone(),
            reads: Arc::new(tokio::sync::Semaphore::new(MAX_HISTORY_READERS)),
        };
        let runtime = RuntimeInterface {
            sms_display_time: config.sms.display_time.wire(),
            hub: hub.clone(),
            commands: receiver,
            access_readings: std::collections::HashMap::new(),
            maca_readings: std::collections::HashMap::new(),
            radio_history: snapshot::RadioHistory::default(),
        };
        (hub, runtime)
    }
    #[cfg(test)]
    pub(crate) fn snapshot(&self) -> TestSnapshot {
        let mut published = (**self.snapshots.borrow()).clone();
        let observed_phones = self.protocol.annotate(&mut published.subscribers);
        TestSnapshot {
            phase: published.snapshot.phase(),
            subscribers: published.subscribers,
            calls: published.snapshot.calls,
            recent_calls: published.recent_calls,
            radio: published.snapshot.radio,
            air_messages: self.protocol.messages(),
            observed_phones,
        }
    }
    pub fn event_layer(&self) -> EventLayer {
        EventLayer(self.events.clone())
    }
}
pub(crate) fn call_phase(
    state: crate::core::call_session::CallState,
) -> (proto::CallPhase, String) {
    use crate::core::call_session::CallState;
    match state {
        CallState::Establishing => (proto::CallPhase::Establishing, String::new()),
        CallState::Alerting => (proto::CallPhase::Alerting, String::new()),
        CallState::WaitingAnswer => (proto::CallPhase::WaitingAnswer, String::new()),
        CallState::Connecting => (proto::CallPhase::Connecting, String::new()),
        CallState::Connected => (proto::CallPhase::Connected, String::new()),
        CallState::Releasing(reason) => (proto::CallPhase::Releasing, end_reason(reason)),
        CallState::Stopped(reason) => (proto::CallPhase::Ended, end_reason(reason)),
    }
}
fn end_reason(reason: crate::core::call_session::EndReason) -> String {
    use crate::core::call_session::{EndReason, Party};
    let phone = |party| match party {
        Party::Caller => "caller",
        Party::Callee => "called phone",
    };
    match reason {
        EndReason::SetupTimeout => "Call setup timed out".into(),
        EndReason::AlertTimeout => "Called phone did not start ringing".into(),
        EndReason::AnswerTimeout => "No answer".into(),
        EndReason::Cancelled => "Cancelled".into(),
        EndReason::MobileRelease(party) => format!("Ended by the {}", phone(party)),
        EndReason::RadioFailure(party) => format!("Radio link to the {} failed", phone(party)),
        EndReason::TimingOutOfRange(party) => {
            format!("The {} moved out of timing range", phone(party))
        }
        EndReason::TimingOverlap => "Bursts from the two phones overlapped".into(),
    }
}
pub(crate) fn bearer_phase(state: crate::core::call_session::BearerState) -> proto::BearerPhase {
    use crate::core::call_session::BearerState;
    match state {
        BearerState::Reserved => proto::BearerPhase::Reserved,
        BearerState::Assigning => proto::BearerPhase::Assigning,
        BearerState::Established => proto::BearerPhase::Established,
        BearerState::Stopping => proto::BearerPhase::Stopping,
        BearerState::Stopped => proto::BearerPhase::Stopped,
    }
}
pub(crate) fn metric(name: impl Into<String>, value: impl ToString, unit: &str) -> proto::Metric {
    proto::Metric {
        name: name.into(),
        value: Some(value.to_string()),
        unit: unit.into(),
        unavailable_reason: String::new(),
    }
}
pub(crate) fn unavailable(name: &str, reason: &str) -> proto::Metric {
    proto::Metric {
        name: name.into(),
        unavailable_reason: reason.into(),
        ..Default::default()
    }
}
pub(crate) fn unix_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| i64::try_from(duration.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}
