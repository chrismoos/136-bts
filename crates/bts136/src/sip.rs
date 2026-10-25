mod config;
#[cfg(feature = "sip-backend")]
mod native;
#[cfg(feature = "sip-backend")]
mod pcm;
pub use config::SipConfig;
pub(crate) use config::number;
use crossbeam_queue::ArrayQueue;
use std::sync::{Arc, mpsc};
pub const PCM_SAMPLES: usize = 160;
const AUDIO_CAPACITY: usize = 4;
const EVENT_CAPACITY: usize = 64;
#[cfg_attr(not(feature = "sip-backend"), allow(dead_code))]
#[derive(Clone)]
pub(crate) enum Digit {
    Start(u8),
    Stop,
    Burst(Vec<u8>),
}
pub(crate) type Pcm = [i16; PCM_SAMPLES];
#[derive(Clone)]
pub(crate) struct Audio {
    pub uplink: Arc<ArrayQueue<Pcm>>,
    pub downlink: Arc<ArrayQueue<Pcm>>,
    pub digits: Arc<ArrayQueue<Digit>>,
}
impl Default for Audio {
    fn default() -> Self {
        Self {
            uplink: Arc::new(ArrayQueue::new(AUDIO_CAPACITY)),
            downlink: Arc::new(ArrayQueue::new(AUDIO_CAPACITY)),
            digits: Arc::new(ArrayQueue::new(EVENT_CAPACITY)),
        }
    }
}
#[cfg_attr(not(feature = "sip-backend"), allow(dead_code))]
#[derive(Debug)]
pub(crate) enum Event {
    Inbound {
        id: String,
        called: String,
        caller: String,
    },
    Ringing {
        id: String,
        early: bool,
    },
    Answered(String),
    Ended {
        id: String,
        status: u16,
    },
    Failed(String),
}
#[cfg_attr(not(feature = "sip-backend"), allow(dead_code))]
pub(crate) enum Command {
    Dial {
        id: String,
        called: String,
        caller: String,
        audio: Audio,
    },
    Attach {
        id: String,
        audio: Audio,
    },
    Ringing(String),
    Answer(String),
    Release {
        id: String,
        status: u16,
    },
    Stop,
}
pub(crate) struct Gateway {
    commands: mpsc::SyncSender<Command>,
    events: mpsc::Receiver<Event>,
    worker: Option<std::thread::JoinHandle<()>>,
}
impl Gateway {
    pub(crate) fn start(config: SipConfig) -> Result<Self, Box<dyn std::error::Error>> {
        config.validate()?;
        #[cfg(feature = "sip-backend")]
        {
            native::start(config)
        }
        #[cfg(not(feature = "sip-backend"))]
        {
            let _ = config;
            Err("SIP backend is not compiled".into())
        }
    }
    pub(crate) fn send(&self, command: Command) -> Result<(), &'static str> {
        self.commands
            .try_send(command)
            .map_err(|_| "SIP command queue unavailable")
    }
    pub(crate) fn event(&self) -> Option<Event> {
        self.events.try_recv().ok()
    }
}
impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
pub(crate) fn test_gateway() -> (Gateway, mpsc::Receiver<Command>, mpsc::SyncSender<Event>) {
    let (commands, receiver) = mpsc::sync_channel(EVENT_CAPACITY);
    let (sender, events) = mpsc::sync_channel(EVENT_CAPACITY);
    (
        Gateway {
            commands,
            events,
            worker: None,
        },
        receiver,
        sender,
    )
}
