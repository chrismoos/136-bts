use super::{
    Command, Hub, PendingCommand,
    proto::{
        self,
        operations_server::{Operations, OperationsServer},
    },
};
use axum::{
    body::Body,
    http::{HeaderMap, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use rust_embed::RustEmbed;
use std::{
    future::IntoFuture,
    net::{SocketAddr, TcpListener},
    sync::mpsc,
    thread,
};
use tokio::sync::oneshot;
use tokio_stream::{StreamExt, wrappers::WatchStream};
use tonic::{Request, Response as RpcResponse, Status};
use tower::Layer;
const COMMAND_TIMEOUT_SECONDS: u64 = 5;
const MAX_REQUEST_ID_BYTES: usize = 128;
const MAX_SMS_USER_OCTETS: usize = crate::l3::sms::MAX_SINGLE_RELAY_USER_OCTETS;
const WEB_WORKER_THREADS: usize = 2;
const DEFAULT_PAGE_SIZE: usize = 50;
const MAX_PAGE_SIZE: usize = 200;
fn page_size(limit: u32) -> Result<usize, Status> {
    match limit as usize {
        0 => Ok(DEFAULT_PAGE_SIZE),
        limit @ 1..=MAX_PAGE_SIZE => Ok(limit),
        _ => Err(Status::invalid_argument(format!(
            "Page size must be at most {MAX_PAGE_SIZE}"
        ))),
    }
}
fn page<T>(rows: Vec<T>, offset: u32, limit: u32) -> Result<(Vec<T>, u32), Status> {
    let limit = page_size(limit)?;
    let total = rows.len() as u32;
    Ok((
        rows.into_iter().skip(offset as usize).take(limit).collect(),
        total,
    ))
}
#[derive(RustEmbed)]
#[folder = "../../ui/dist/"]
struct Assets;
#[derive(Clone)]
struct Service(Hub);
#[tonic::async_trait]
impl Operations for Service {
    async fn get_snapshot(
        &self,
        _: Request<proto::Empty>,
    ) -> Result<RpcResponse<proto::Snapshot>, Status> {
        let published = self.0.snapshots.borrow().clone();
        Ok(RpcResponse::new(self.status_snapshot(&published).await))
    }
    type WatchStream =
        std::pin::Pin<Box<dyn tokio_stream::Stream<Item = Result<proto::Snapshot, Status>> + Send>>;
    async fn watch(
        &self,
        _: Request<proto::Empty>,
    ) -> Result<RpcResponse<Self::WatchStream>, Status> {
        let service = self.clone();
        let stream = WatchStream::new(self.0.snapshots.subscribe()).then(move |value| {
            let service = service.clone();
            async move { Ok(service.status_snapshot(&value).await) }
        });
        Ok(RpcResponse::new(Box::pin(stream)))
    }
    async fn list_signaling(
        &self,
        request: Request<proto::SignalingQuery>,
    ) -> Result<RpcResponse<proto::SignalingPage>, Status> {
        let mut query = request.into_inner();
        let published = self.0.snapshots.borrow().clone();
        if !query.number.is_empty() {
            let Some(subscriber) = published
                .subscribers
                .iter()
                .find(|subscriber| subscriber.number == query.number)
            else {
                return Ok(RpcResponse::new(proto::SignalingPage::default()));
            };
            query.identity = subscriber.identity.clone();
        }
        let mut page = self.0.protocol.query(query)?;
        for message in &mut page.messages {
            message.number = published
                .subscribers
                .iter()
                .find(|subscriber| subscriber.identity == message.identity)
                .map(|subscriber| subscriber.number.clone())
                .unwrap_or_default();
        }
        Ok(RpcResponse::new(page))
    }
    async fn list_messages(
        &self,
        request: Request<proto::ListMessagesRequest>,
    ) -> Result<RpcResponse<proto::MessagePage>, Status> {
        let request = request.into_inner();
        let limit = page_size(request.limit)?;
        if request.before_id.is_some_and(|id| id <= 0) {
            return Err(Status::invalid_argument("before_id must be positive"));
        }
        let phase = match proto::SmsPhase::try_from(request.phase) {
            Ok(proto::SmsPhase::Unspecified) => None,
            Ok(phase) => Some(phase),
            Err(_) => return Err(Status::invalid_argument("Unknown SMS phase")),
        };
        let filter = crate::core::store::MessageFilter {
            before_id: request.before_id,
            phase,
            number: (!request.number.is_empty()).then_some(request.number),
        };
        let offset = request.offset as usize;
        let (mut messages, total) = self
            .read_store(move |store| store.operation_messages_page(&filter, offset, limit))
            .await?;
        let published = self.0.snapshots.borrow().clone();
        for message in &mut messages {
            if let Some(live) = published
                .live_messages
                .iter()
                .find(|live| live.id == message.id)
            {
                message
                    .measurements
                    .extend(live.measurements.iter().cloned());
            }
        }
        Ok(RpcResponse::new(proto::MessagePage {
            messages,
            total: total as u32,
        }))
    }
    async fn list_subscribers(
        &self,
        request: Request<proto::SubscriberQuery>,
    ) -> Result<RpcResponse<proto::SubscriberPage>, Status> {
        let request = request.into_inner();
        let sort = proto::SubscriberSort::try_from(request.sort)
            .map_err(|_| Status::invalid_argument("Unknown subscriber sort"))?;
        let mut subscribers = self.0.snapshots.borrow().subscribers.clone();
        self.0.protocol.annotate(&mut subscribers);
        let search = request.search.to_lowercase();
        let esn = request
            .identity
            .strip_prefix("ESN(")
            .and_then(|rest| rest.strip_suffix(')'))
            .and_then(|value| value.parse::<u32>().ok());
        subscribers.retain(|subscriber| {
            (request.identity.is_empty()
                || subscriber.identity == request.identity
                || esn.is_some() && subscriber.provisioned_esn == esn)
                && (request.number.is_empty() || subscriber.number == request.number)
                && (search.is_empty()
                    || format!(
                        "{} {} {:08x}",
                        subscriber.number,
                        subscriber.identity,
                        subscriber
                            .provisioned_esn
                            .or(subscriber.esn)
                            .unwrap_or_default()
                    )
                    .to_lowercase()
                    .contains(&search))
        });
        subscribers.sort_by(|a, b| {
            let order = match sort {
                proto::SubscriberSort::Number => {
                    (a.number.len(), &a.number).cmp(&(b.number.len(), &b.number))
                }
                proto::SubscriberSort::Identity => a.identity.cmp(&b.identity),
                proto::SubscriberSort::Registration => a.registered.cmp(&b.registered),
            };
            if request.descending {
                order.reverse()
            } else {
                order
            }
        });
        let (subscribers, total) = page(subscribers, request.offset, request.limit)?;
        Ok(RpcResponse::new(proto::SubscriberPage {
            subscribers,
            total,
        }))
    }
    async fn list_observed_phones(
        &self,
        request: Request<proto::ObservedPhoneQuery>,
    ) -> Result<RpcResponse<proto::ObservedPhonePage>, Status> {
        let request = request.into_inner();
        let mut subscribers = self.0.snapshots.borrow().subscribers.clone();
        let mut phones = self.0.protocol.annotate(&mut subscribers);
        phones.retain(|phone| {
            (request.identity.is_empty() || phone.identity == request.identity)
                && !subscribers
                    .iter()
                    .any(|subscriber| subscriber.identity == phone.identity)
        });
        let (phones, total) = page(phones, request.offset, request.limit)?;
        Ok(RpcResponse::new(proto::ObservedPhonePage { phones, total }))
    }
    async fn list_calls(
        &self,
        request: Request<proto::CallQuery>,
    ) -> Result<RpcResponse<proto::CallPage>, Status> {
        let request = request.into_inner();
        let mut calls = self.0.snapshots.borrow().recent_calls.clone();
        if !request.identity.is_empty() {
            calls.retain(|call| call.legs.iter().any(|leg| leg.identity == request.identity));
        }
        let (calls, total) = page(calls, request.offset, request.limit)?;
        Ok(RpcResponse::new(proto::CallPage { calls, total }))
    }
    async fn list_events(
        &self,
        request: Request<proto::EventQuery>,
    ) -> Result<RpcResponse<proto::EventPage>, Status> {
        let request = request.into_inner();
        let limit = page_size(request.limit)?;
        self.0.events.query(request, limit).map(RpcResponse::new)
    }
    async fn save_subscriber(
        &self,
        request: Request<proto::SaveSubscriberRequest>,
    ) -> Result<RpcResponse<proto::CommandResult>, Status> {
        self.command(Command::SaveSubscriber(request.into_inner()))
            .await
            .map(RpcResponse::new)
    }
    async fn delete_subscriber(
        &self,
        request: Request<proto::SubscriberIdentity>,
    ) -> Result<RpcResponse<proto::CommandResult>, Status> {
        self.command(Command::DeleteSubscriber(request.into_inner()))
            .await
            .map(RpcResponse::new)
    }
    async fn send_sms(
        &self,
        request: Request<proto::SendSmsRequest>,
    ) -> Result<RpcResponse<proto::CommandResult>, Status> {
        let request = request.into_inner();
        if request.request_id.is_empty()
            || request.request_id.len() > MAX_REQUEST_ID_BYTES
            || request.text.is_empty()
        {
            return Err(Status::invalid_argument(
                "Supply a request ID and nonempty SMS text",
            ));
        }
        if request.sender_number.is_empty()
            || !request
                .sender_number
                .bytes()
                .all(|digit| digit.is_ascii_digit())
        {
            return Err(Status::invalid_argument(
                "Supply a numeric SMS sender address",
            ));
        }
        let user_data = crate::l3::sms::UserData::from_text(&request.text).map_err(|error| {
            Status::invalid_argument(format!("SMS text must fit {MAX_SMS_USER_OCTETS} encoded octets and use BMP characters: {error}"))
        })?;
        if user_data
            .octet_len()
            .map_err(|error| Status::invalid_argument(error.to_string()))?
            > MAX_SMS_USER_OCTETS
        {
            return Err(Status::invalid_argument(format!(
                "SMS text exceeds {MAX_SMS_USER_OCTETS} encoded octets"
            )));
        }
        self.command(Command::Sms(request))
            .await
            .map(RpcResponse::new)
    }
    async fn set_measurements(
        &self,
        request: Request<proto::SetMeasurementsRequest>,
    ) -> Result<RpcResponse<proto::CommandResult>, Status> {
        let request = request.into_inner();
        proto::CallParty::try_from(request.party)
            .map_err(|_| Status::invalid_argument("Unknown call party"))?;
        if request.call_id == 0 {
            return Err(Status::invalid_argument("Call ID must be positive"));
        }
        super::extended_measurements::validate(&request)?;
        self.command(Command::Measurements(request))
            .await
            .map(RpcResponse::new)
    }
    async fn send_user_alert(
        &self,
        request: Request<proto::SendUserAlertRequest>,
    ) -> Result<RpcResponse<proto::CommandResult>, Status> {
        let request = request.into_inner();
        super::user_alert::fields(&request)?;
        self.command(Command::UserAlert(request))
            .await
            .map(RpcResponse::new)
    }
    async fn send_message_waiting(
        &self,
        request: Request<proto::SendMessageWaitingRequest>,
    ) -> Result<RpcResponse<proto::CommandResult>, Status> {
        let request = request.into_inner();
        super::snapshot::message_waiting_fields(&request)?;
        self.command(Command::MessageWaiting(request))
            .await
            .map(RpcResponse::new)
    }
    async fn audit_subscriber(
        &self,
        request: Request<proto::AuditSubscriberRequest>,
    ) -> Result<RpcResponse<proto::CommandResult>, Status> {
        let request = request.into_inner();
        super::snapshot::subscriber_identity(
            request
                .subscriber_id
                .ok_or_else(|| Status::invalid_argument("Subscriber identity is required"))?,
        )?;
        self.command(Command::Audit(request))
            .await
            .map(RpcResponse::new)
    }
    async fn request_capabilities(
        &self,
        request: Request<proto::SubscriberIdentity>,
    ) -> Result<RpcResponse<proto::CommandResult>, Status> {
        let request = request.into_inner();
        super::snapshot::subscriber_identity(request)?;
        self.command(Command::Capabilities(request))
            .await
            .map(RpcResponse::new)
    }
    async fn request_status(
        &self,
        request: Request<proto::RequestStatusRequest>,
    ) -> Result<RpcResponse<proto::CommandResult>, Status> {
        let request = request.into_inner();
        proto::CallParty::try_from(request.party)
            .map_err(|_| Status::invalid_argument("Unknown call party"))?;
        if request.call_id == 0 {
            return Err(Status::invalid_argument("Call ID must be positive"));
        }
        self.command(Command::Status(request))
            .await
            .map(RpcResponse::new)
    }
    async fn send_call_information(
        &self,
        request: Request<proto::SendCallInformationRequest>,
    ) -> Result<RpcResponse<proto::CommandResult>, Status> {
        let request = request.into_inner();
        super::call_information::fields(&request)?;
        self.command(Command::CallInformation(request))
            .await
            .map(RpcResponse::new)
    }
    async fn set_physical_control(
        &self,
        request: Request<proto::SetPhysicalControlRequest>,
    ) -> Result<RpcResponse<proto::CommandResult>, Status> {
        let request = request.into_inner();
        proto::CallParty::try_from(request.party)
            .map_err(|_| Status::invalid_argument("Unknown call party"))?;
        if request.call_id == 0 {
            return Err(Status::invalid_argument("Call ID must be positive"));
        }
        if request.mobile_power_level.is_none()
            && request.timing_advance_half_symbols.is_none()
            && request.automatic_timing.is_none()
        {
            return Err(Status::invalid_argument(
                "Supply mobile power level, timing advance or automatic timing mode",
            ));
        }
        if request
            .mobile_power_level
            .is_some_and(|value| value > u32::from(crate::l3::legacy_traffic::MAX_POWER_LEVEL))
            || request.timing_advance_half_symbols.is_some_and(|value| {
                value > u32::from(crate::l3::legacy_traffic::MAX_ALIGNMENT_UNITS)
            })
        {
            return Err(Status::invalid_argument(
                "Mobile power level must be 0–10 and timing advance must be 0–30 half-symbols",
            ));
        }
        if request.automatic_timing == Some(true) && request.timing_advance_half_symbols.is_some() {
            return Err(Status::invalid_argument(
                "Automatic timing and an explicit timing advance cannot be combined",
            ));
        }
        self.command(Command::PhysicalControl(request))
            .await
            .map(RpcResponse::new)
    }
    async fn start_call(
        &self,
        request: Request<proto::StartCallRequest>,
    ) -> Result<RpcResponse<proto::CommandResult>, Status> {
        let request = request.into_inner();
        super::snapshot::subscriber_identity(
            request
                .subscriber_id
                .ok_or_else(|| Status::invalid_argument("Subscriber identity is required"))?,
        )?;
        if request.calling_number.is_empty()
            || !request
                .calling_number
                .bytes()
                .all(|digit| digit.is_ascii_digit())
        {
            return Err(Status::invalid_argument(
                "Calling number must contain telephone digits",
            ));
        }
        crate::l3::legacy_traffic::CallingNumber::network(&request.calling_number).map_err(
            |_| Status::invalid_argument("Calling number must be a valid telephone number"),
        )?;
        self.command(Command::StartCall(request))
            .await
            .map(RpcResponse::new)
    }
    async fn release_call(
        &self,
        request: Request<proto::ReleaseCallRequest>,
    ) -> Result<RpcResponse<proto::CommandResult>, Status> {
        self.command(Command::Release(request.into_inner()))
            .await
            .map(RpcResponse::new)
    }
}
impl Service {
    async fn status_snapshot(&self, published: &super::Published) -> proto::Snapshot {
        let mut snapshot = published.snapshot.clone();
        snapshot.last_uplink_unix_ms = self.0.protocol.last_uplink_unix_ms();
        snapshot.queued_sms = self
            .read_store(|store| store.queued_message_count())
            .await
            .ok()
            .map(|count| count as u32);
        snapshot
    }
    async fn read_store<T: Send + 'static>(
        &self,
        read: impl FnOnce(&crate::core::store::StateStore) -> Result<T, crate::core::store::StoreError>
        + Send
        + 'static,
    ) -> Result<T, Status> {
        let permit = self
            .0
            .reads
            .clone()
            .try_acquire_owned()
            .map_err(|_| Status::resource_exhausted("Too many SMS store reads"))?;
        let path = self.0.state_path.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let store = crate::core::store::StateStore::open_read_only(path)
                .map_err(|error| Status::unavailable(error.to_string()))?;
            read(&store).map_err(|error| Status::internal(error.to_string()))
        })
        .await
        .map_err(|_| Status::internal("SMS store read failed"))?
    }

    async fn command(&self, command: Command) -> Result<proto::CommandResult, Status> {
        let unknown_outcome = match command {
            Command::Sms(_) => {
                "The cell did not answer in time, so the SMS may or may not be queued. Retry with the same request ID to avoid sending it twice"
            }
            Command::StartCall(_) => {
                "The cell did not answer in time, so the call may still start. Check Calls in progress before retrying"
            }
            _ => {
                "The cell did not answer in time, so the command may still take effect. Check the cell state before retrying"
            }
        };
        let (response, receiver) = oneshot::channel();
        self.0
            .commands
            .try_send(PendingCommand { command, response })
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    Status::resource_exhausted("Cell command queue is full")
                }
                mpsc::TrySendError::Disconnected(_) => {
                    Status::unavailable("Cell runtime has stopped")
                }
            })?;
        tokio::time::timeout(
            std::time::Duration::from_secs(COMMAND_TIMEOUT_SECONDS),
            receiver,
        )
        .await
        .map_err(|_| Status::deadline_exceeded(unknown_outcome))?
        .map_err(|_| Status::unavailable("Cell runtime has stopped"))?
    }
}
async fn asset(uri: Uri) -> Response {
    let path = uri.path().trim_start_matches('/');
    let path = if path.is_empty() { "index.html" } else { path };
    let Some(asset) = Assets::get(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = if path.ends_with(".html") {
        "text/html; charset=utf-8"
    } else if path.ends_with(".js") {
        "text/javascript; charset=utf-8"
    } else if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if path.ends_with(".woff2") {
        "font/woff2"
    } else if path.ends_with(".woff") {
        "font/woff"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else {
        "application/octet-stream"
    };
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        Body::from(asset.data.into_owned()),
    )
        .into_response()
}
async fn same_origin(
    axum::extract::State(origins): axum::extract::State<std::sync::Arc<Vec<String>>>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if !origin_allowed(request.headers(), &origins) {
        return StatusCode::FORBIDDEN.into_response();
    }
    next.run(request).await
}
fn origin_allowed(headers: &HeaderMap, allowed: &[String]) -> bool {
    let Some(origin) = headers.get(header::ORIGIN) else {
        return true;
    };
    let Some(host) = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    if allowed.iter().any(|allowed| allowed == origin) {
        return true;
    }
    origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
        .is_some_and(|authority| authority == host)
}
pub struct Server {
    stop: Option<oneshot::Sender<()>>,
    thread: Option<thread::JoinHandle<Result<(), std::io::Error>>>,
    address: SocketAddr,
}
impl Server {
    pub fn start(address: SocketAddr, hub: Hub) -> Result<Self, std::io::Error> {
        Self::start_with_origins(address, hub, Vec::new())
    }
    pub fn start_with_origins(
        address: SocketAddr,
        hub: Hub,
        allowed_origins: Vec<String>,
    ) -> Result<Self, std::io::Error> {
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let (stop, stopped) = oneshot::channel();
        let thread = thread::Builder::new()
            .name("136bts-web".into())
            .spawn(move || {
                let _protocol_worker = hub.protocol.start_worker();
                let runtime = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(WEB_WORKER_THREADS)
                    .enable_all()
                    .build()?;
                runtime.block_on(async move {
                    let service =
                        tonic_web::GrpcWebLayer::new().layer(OperationsServer::new(Service(hub)));
                    let router = tonic::service::Routes::new(service)
                        .into_axum_router()
                        .fallback(asset)
                        .layer(axum::middleware::from_fn_with_state(
                            std::sync::Arc::new(allowed_origins),
                            same_origin,
                        ));
                    let serve = axum::serve(tokio::net::TcpListener::from_std(listener)?, router)
                        .into_future();
                    tokio::select! {
                        result = serve => result,
                        _ = stopped => Ok(()),
                    }
                })
            })?;
        Ok(Self {
            stop: Some(stop),
            thread: Some(thread),
            address,
        })
    }
    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn stop(mut self) -> Result<(), std::io::Error> {
        self.request_stop();
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| std::io::Error::other("Web server thread panicked"))??;
        }
        Ok(())
    }
    fn request_stop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.request_stop();
    }
}

#[cfg(test)]
mod tests;
