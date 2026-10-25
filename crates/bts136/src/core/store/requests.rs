//! Store access for threads that do not own the state store.
//!
//! A request runs on the thread that owns the store, and its answer is sent only once the
//! changes it made are on disk, so a peer that acknowledges after the answer never
//! acknowledges something a crash could lose.

use super::StateStore;
use std::{collections::VecDeque, fmt, sync::mpsc, thread};

type Responder = Box<dyn FnOnce() + Send>;
type Request = Box<dyn FnOnce(&mut StateStore) -> Responder + Send>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoreClosed;
impl fmt::Display for StoreClosed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("state store is no longer running")
    }
}
impl std::error::Error for StoreClosed {}

#[derive(Clone)]
pub struct StoreClient {
    requests: mpsc::Sender<Request>,
}

impl StoreClient {
    pub async fn call<R: Send + 'static>(
        &self,
        work: impl FnOnce(&mut StateStore) -> R + Send + 'static,
    ) -> Result<R, StoreClosed> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        let request: Request = Box::new(move |store| {
            let value = work(store);
            Box::new(move || {
                let _ = reply.send(value);
            })
        });
        self.requests.send(request).map_err(|_| StoreClosed)?;
        answer.await.map_err(|_| StoreClosed)
    }

    /// Serves requests from a thread of its own, for a store nothing else uses.
    pub fn spawn_direct(mut store: StateStore) -> std::io::Result<Self> {
        let (client, mut requests) = StoreRequests::channel();
        thread::Builder::new()
            .name("136bts-store-requests".into())
            .spawn(move || {
                while let Ok(request) = requests.requests.recv() {
                    requests.run(&mut store, request);
                    while !requests.waiting.is_empty() {
                        if store.flush().is_err() {
                            break;
                        }
                        requests.answer_durable(&store);
                        thread::yield_now();
                    }
                }
            })?;
        Ok(client)
    }
}

pub struct StoreRequests {
    requests: mpsc::Receiver<Request>,
    waiting: VecDeque<(u64, Responder)>,
}

impl StoreRequests {
    pub fn channel() -> (StoreClient, Self) {
        let (sender, requests) = mpsc::channel();
        (
            StoreClient { requests: sender },
            Self {
                requests,
                waiting: VecDeque::new(),
            },
        )
    }

    /// Runs at most `limit` waiting requests without blocking. Their answers are held
    /// until `answer_durable` sees the changes on disk.
    pub fn run_pending(&mut self, store: &mut StateStore, limit: usize) {
        for _ in 0..limit {
            let Ok(request) = self.requests.try_recv() else {
                break;
            };
            self.run(store, request);
        }
    }

    pub fn answer_durable(&mut self, store: &StateStore) {
        while let Some((mark, _)) = self.waiting.front() {
            if !store.is_durable(*mark) {
                break;
            }
            if let Some((_, responder)) = self.waiting.pop_front() {
                responder();
            }
        }
    }

    fn run(&mut self, store: &mut StateStore, request: Request) {
        let responder = request(store);
        self.waiting.push_back((store.change_mark(), responder));
    }
}
