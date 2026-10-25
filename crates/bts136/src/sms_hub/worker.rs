use super::*;
use crate::core::store::{StoreError, requests::StoreClient, sms_hub::HubError};
use smpp_core::tcp::{Connector, Endpoint};
use smpp_core::{Action, CommandStatus, Connection, ConnectionOptions, State};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::net::TcpStream;
const POLL_INTERVAL: Duration = Duration::from_millis(200);
const IO_TIMEOUT: Duration = Duration::from_secs(3);
const WINDOW: usize = 16;
#[derive(Clone, Debug)]
pub(crate) struct Status {
    pub state: &'static str,
    pub peer: String,
    pub pending: usize,
    pub error: Option<String>,
}
pub(crate) type StatusHandle = Arc<Mutex<Status>>;
pub(crate) struct Gateway {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    pub status: StatusHandle,
}
impl Gateway {
    pub(crate) fn start(config: SmsHubConfig, store: StoreClient) -> std::io::Result<Self> {
        let status = Arc::new(Mutex::new(Status {
            state: if config.enabled {
                "Starting"
            } else {
                "Disabled"
            },
            peer: if config.role == BindRole::Client {
                config.hub_addr.to_string()
            } else {
                config.listen_addr.to_string()
            },
            pending: 0,
            error: None,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = if config.enabled {
            let stop = stop.clone();
            let status = status.clone();
            Some(
                std::thread::Builder::new()
                    .name("136bts-smpp".into())
                    .spawn(move || {
                        match tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                        {
                            Ok(runtime) => runtime.block_on(run(config, store, stop, &status)),
                            Err(error) => update(&status, "Failed", 0, Some(error.to_string())),
                        }
                    })?,
            )
        } else {
            None
        };
        Ok(Self {
            stop,
            worker,
            status,
        })
    }
}
impl Gateway {
    pub(crate) fn request_stop(&self) {
        self.stop.store(true, Ordering::Release);
    }
}
impl Drop for Gateway {
    fn drop(&mut self) {
        self.request_stop();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
fn update(status: &StatusHandle, state: &'static str, pending: usize, error: Option<String>) {
    let mut current = status.lock().unwrap_or_else(|e| e.into_inner());
    current.state = state;
    current.pending = pending;
    current.error = error;
}
fn now() -> Result<i64, std::io::Error> {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(std::io::Error::other)?
            .as_millis(),
    )
    .map_err(std::io::Error::other)
}
fn io(error: impl std::error::Error + Send + Sync + 'static) -> std::io::Error {
    std::io::Error::other(error)
}
async fn run(
    config: SmsHubConfig,
    store: StoreClient,
    stop: Arc<AtomicBool>,
    status: &StatusHandle,
) {
    let endpoint = match config.role {
        BindRole::Client => Endpoint::Client(config.hub_addr),
        BindRole::Server => Endpoint::Server(config.listen_addr),
    };
    let mut connector = Connector::new(endpoint);
    while !stop.load(Ordering::Acquire) {
        let result = async {
            let depth = store
                .call(|store| store.hub_queue_depth())
                .await
                .map_err(io)?
                .map_err(io)?;
            update(
                status,
                if config.role == BindRole::Server {
                    "Listening"
                } else {
                    "Connecting"
                },
                depth,
                None,
            );
            let Some(stream) = connector.next(|| stop.load(Ordering::Acquire)).await? else {
                return Ok(());
            };
            connection(&store, &config, stream, &stop, status).await
        }
        .await;
        if let Err(error) = result {
            tracing::warn!(%error,"SMPP connection ended. External SMS remains queued");
            let depth = status
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .pending;
            update(status, "Backoff", depth, Some(error.to_string()));
        }
    }
    update(status, "Stopped", 0, None);
}
fn equal_secret(a: &[u8], b: &[u8]) -> bool {
    let mut different = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        different |= usize::from(
            a.get(i).copied().unwrap_or_default() ^ b.get(i).copied().unwrap_or_default(),
        );
    }
    different == 0
}
fn rejected(error: &StoreError) -> CommandStatus {
    match error {
        StoreError::Hub(HubError::QueueFull) => CommandStatus::MessageQueueFull,
        StoreError::Hub(HubError::Address | HubError::Receipt) => CommandStatus::InvalidDestAddress,
        StoreError::Hub(HubError::Encoding | HubError::Length | HubError::Options)
        | StoreError::Sms(_) => CommandStatus::InvalidMsgLength,
        StoreError::IdempotencyConflict => CommandStatus::SubmitFailed,
        _ => CommandStatus::SystemError,
    }
}
async fn connection(
    store: &StoreClient,
    config: &SmsHubConfig,
    stream: TcpStream,
    stop: &AtomicBool,
    status: &StatusHandle,
) -> std::io::Result<()> {
    let mut session = Connection::new(
        stream,
        if config.role == BindRole::Server {
            smpp_core::Config::smsc(&config.system_id)
        } else {
            smpp_core::Config::esme(&config.system_id, &config.password)
        },
        ConnectionOptions {
            bind_timeout: IO_TIMEOUT,
            ..Default::default()
        },
    );
    let mut pending = HashMap::new();
    let mut interval = tokio::time::interval(POLL_INTERVAL);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let peer = format!(
        "{:?}:{}:{}",
        config.role,
        if config.role == BindRole::Client {
            config.hub_addr
        } else {
            config.listen_addr
        },
        config.system_id
    );
    store
        .call(|store| store.recover_hub_sms())
        .await
        .map_err(io)?
        .map_err(io)?;
    update(status, "Binding", 0, None);
    let result: std::io::Result<()> = async {
        loop {
            let actions = tokio::select! {
                received = session.next() => {
                    let Some(action) = received? else { return Ok(()) };
                    vec![action]
                }
                _ = interval.tick() => Vec::new(),
            };
            for action in actions {
                match action {
                    Action::Send(pdu) => session.send(pdu).await?,
                    Action::BindRequest { sequence, system_id, password } => {
                        let valid = equal_secret(system_id.as_bytes(), config.system_id.as_bytes())
                            & equal_secret(password.as_bytes(), config.password.as_bytes());
                        if valid { session.accept_bind(sequence, system_id).await?; }
                        else { session.reject_bind(sequence, CommandStatus::BindFailed).await?; }
                        if !valid {
                            return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "SMPP bind refused"));
                        }
                        tracing::info!("SMPP peer bound. 136BTS role is SMSC");
                        update(status, "Bound", 0, None);
                    }
                    Action::Bound { .. } => {
                        tracing::info!("SMPP peer bound. 136BTS role is ESME");
                        update(status, "Bound", 0, None);
                    }
                    Action::Message { sequence, body } => {
                        let outcome = if body.is_delivery_receipt() {
                            if config.role == BindRole::Client {
                                let (peer, body) = (peer.clone(), body.clone());
                                store
                                    .call(move |store| store.receive_hub_receipt(&peer, &body))
                                    .await
                                    .map_err(io)?
                                    .map(|()| String::new())
                            } else {
                                Err(HubError::Receipt.into())
                            }
                        } else {
                            let (peer, body, received_ms) = (peer.clone(), body.clone(), now()?);
                            store
                                .call(move |store| store.receive_hub_sms(&peer, &body, received_ms))
                                .await
                                .map_err(io)?
                        };
                        let (status, id) = match outcome {
                            Ok(id) => {
                                tracing::info!(source=%body.source.digits, destination=%body.destination.digits, receipt=body.is_delivery_receipt(), local_id=%id, "SMPP inbound message committed");
                                (CommandStatus::Ok, id)
                            }
                            Err(error) => {
                                tracing::warn!(%error, "SMPP inbound message rejected");
                                (rejected(&error), String::new())
                            }
                        };
                        session.send(session.respond_to_message(sequence, status, id)).await?;
                    }
                    Action::MessageAck { sequence, status, message_id } => {
                        if let Some(id) = pending.remove(&sequence) {
                            tracing::info!(job_id=id, status=status.as_u32(), peer_message_id=%message_id, "SMPP message response");
                            let (peer, settled_ms) = (peer.clone(), now()?);
                            store
                                .call(move |store| store.hub_sms_result(id, status, &message_id, &peer, settled_ms))
                                .await
                                .map_err(io)?
                                .map_err(io)?;
                        }
                    }
                    Action::RequestTimeout { .. } => return Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut, "SMPP response timeout. Outcome may be unknown",
                    )),
                    Action::Closed => return Ok(()),
                }
            }
            if stop.load(Ordering::Acquire) {
                if session.state() == State::Bound {
                    session.shutdown().await?;
                }
                return Ok(());
            }
            if session.state() == State::Bound {
                let timestamp = now()?;
                if config.role == BindRole::Server {
                    store
                        .call(move |store| store.queue_hub_reports(timestamp))
                        .await
                        .map_err(io)?
                        .map_err(io)?;
                }
                while session.outstanding() < WINDOW {
                    let (server, job_peer) = (config.role == BindRole::Server, peer.clone());
                    let Some(job) = store
                        .call(move |store| store.next_hub_sms(timestamp, server, &job_peer))
                        .await
                        .map_err(io)?
                        .map_err(io)?
                    else {
                        break;
                    };
                    let sequence = match session.send_message(job.body).await {
                        Ok(sequence) => sequence,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            let (id, retry_ms) = (job.id, now()?);
                            store
                                .call(move |store| store.retry_hub_sms(id, retry_ms))
                                .await
                                .map_err(io)?
                                .map_err(io)?;
                            break;
                        }
                        Err(error) => return Err(error),
                    };
                    pending.insert(sequence, job.id);
                }
                let depth = store
                    .call(|store| store.hub_queue_depth())
                    .await
                    .map_err(io)?
                    .map_err(io)?;
                update(status, "Bound", depth, None);
            }
        }
    }
    .await;
    let shutdown = session.shutdown().await;
    while let Ok(Some(action)) = session.next().await {
        if let Action::MessageAck {
            sequence,
            status,
            message_id,
        } = action
            && let Some(id) = pending.remove(&sequence)
        {
            let (peer, settled_ms) = (peer.clone(), now()?);
            store
                .call(move |store| store.hub_sms_result(id, status, &message_id, &peer, settled_ms))
                .await
                .map_err(io)?
                .map_err(io)?;
        }
    }
    for id in pending.values().copied() {
        let retry_ms = now()?;
        store
            .call(move |store| store.retry_hub_sms(id, retry_ms))
            .await
            .map_err(io)?
            .map_err(io)?;
    }
    result.and(shutdown)
}
#[cfg(test)]
mod tests;
