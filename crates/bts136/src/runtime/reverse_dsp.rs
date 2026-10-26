use crate::{
    phy::{
        acquisition::{
            MatchedFilter,
            reverse_stream::{Burst, ReverseControlStream, ReverseStream},
        },
        modulation::Symbol,
        rach::ReverseControlFields,
        shortened::ShortenedBurst,
        traffic::TrafficFields,
    },
    radio::{
        buffered_receiver::{Feed, FeedBlock},
        receive_window::{Admission, ReceiveWindow},
    },
};
use std::{
    error::Error,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
const FEED_WAIT: Duration = Duration::from_millis(20);

type Failure = Box<dyn Error + Send + Sync>;

#[derive(Debug)]
pub(crate) struct Decoded<T> {
    pub burst: Burst<T>,
    /// RX clock tick of the burst start, extrapolated from the block that completed it.
    pub rx_tick: f64,
}

#[derive(Debug)]
pub(crate) struct Block {
    pub generation: u64,
    pub first_sample: u64,
    pub time_ticks: u64,
    pub admission: Admission,
    pub power_dbfs: Option<f64>,
    pub access: Vec<Decoded<ReverseControlFields>>,
    pub traffic: Vec<Decoded<TrafficFields>>,
    pub shortened: Vec<Decoded<ShortenedBurst>>,
}

enum Event {
    Block(Block),
    Failed(Failure),
}

pub(crate) struct Streams {
    pub control: ReverseControlStream,
    pub traffic: ReverseStream<TrafficFields>,
    pub shortened: ReverseStream<ShortenedBurst>,
    pub max_bursts: usize,
}

pub(crate) struct Clocks {
    pub rx_sample_rate_hz: f64,
    pub rx_tick_rate_hz: f64,
    pub tx_tick: u64,
}

struct Shared {
    stop: AtomicBool,
    tx_tick: AtomicU64,
    generation: AtomicU64,
    traffic: AtomicBool,
    shortened: AtomicBool,
}

pub(crate) struct ReverseDsp {
    shared: Arc<Shared>,
    events: mpsc::Receiver<Event>,
    thread: Option<JoinHandle<()>>,
}

impl ReverseDsp {
    pub fn spawn(
        feed: Feed,
        window: ReceiveWindow,
        streams: Streams,
        clocks: Clocks,
        realtime: crate::realtime::Config,
    ) -> Result<Self, Box<dyn Error>> {
        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            tx_tick: AtomicU64::new(clocks.tx_tick),
            generation: AtomicU64::new(0),
            traffic: AtomicBool::new(false),
            shortened: AtomicBool::new(false),
        });
        let filter = MatchedFilter::try_new(clocks.rx_sample_rate_hz)?;
        let (events_tx, events) = mpsc::channel();
        let mut worker = Worker {
            shared: shared.clone(),
            feed,
            window,
            filter,
            streams,
            clocks,
            generation: 0,
            symbols: Vec::new(),
            filtered: Vec::new(),
        };
        let thread = thread::Builder::new()
            .name("136bts-rx-dsp".into())
            .spawn(move || {
                let _scheduling = crate::realtime::apply(realtime, crate::realtime::Role::Rx);
                while !worker.shared.stop.load(Ordering::Acquire) {
                    let (event, samples) = match worker.feed.recv_timeout(FEED_WAIT) {
                        Ok(None) => continue,
                        Ok(Some(FeedBlock { block, samples })) => {
                            match worker.process(&block, &samples) {
                                Ok(block) => (Event::Block(block), Some(samples)),
                                Err(error) => (Event::Failed(error), None),
                            }
                        }
                        Err(error) => (Event::Failed(Box::new(error)), None),
                    };
                    let failed = matches!(event, Event::Failed(_));
                    if events_tx.send(event).is_err() || failed {
                        break;
                    }
                    // The buffer returns only once the decoded block is queued for the serving loop.
                    if let Some(samples) = samples
                        && let Err(error) = worker.feed.recycle(samples)
                    {
                        let _ = events_tx.send(Event::Failed(Box::new(error)));
                        break;
                    }
                }
            })?;
        Ok(Self {
            shared,
            events,
            thread: Some(thread),
        })
    }
    pub fn publish_tx_tick(&self, tick: u64) {
        self.shared.tx_tick.store(tick, Ordering::Release);
    }
    /// Discards retained IQ before the next block. Blocks from earlier generations remain queued and must be dropped.
    pub fn reset(&self) -> u64 {
        self.shared.generation.fetch_add(1, Ordering::AcqRel) + 1
    }
    pub fn set_traffic(&self, enabled: bool) {
        self.shared.traffic.store(enabled, Ordering::Release);
    }
    pub fn set_shortened(&self, enabled: bool) {
        self.shared.shortened.store(enabled, Ordering::Release);
    }
    pub fn recv_timeout(&self, timeout: Duration) -> Result<Option<Block>, Box<dyn Error>> {
        match self.events.recv_timeout(timeout) {
            Ok(Event::Block(block)) => Ok(Some(block)),
            Ok(Event::Failed(error)) => Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err("RX DSP thread stopped".into()),
        }
    }
    /// Joins the thread and releases the RX feed. Call before stopping the radio.
    pub fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::error!("RX DSP thread panicked");
        }
    }
}

impl Drop for ReverseDsp {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Worker {
    shared: Arc<Shared>,
    feed: Feed,
    window: ReceiveWindow,
    filter: MatchedFilter,
    streams: Streams,
    clocks: Clocks,
    generation: u64,
    symbols: Vec<Symbol>,
    filtered: Vec<Symbol>,
}

impl Worker {
    fn discontinuity(&mut self) {
        self.filter.reset();
        self.streams.control.discontinuity();
        self.streams.traffic.discontinuity();
        self.streams.shortened.discontinuity();
    }
    fn process(
        &mut self,
        block: &crate::radio::receiver::Block,
        samples: &[num_complex::Complex32],
    ) -> Result<Block, Failure> {
        let generation = self.shared.generation.load(Ordering::Acquire);
        if generation != self.generation {
            self.generation = generation;
            self.discontinuity();
        }
        let admission = self
            .window
            .admit(block, self.shared.tx_tick.load(Ordering::Acquire))?;
        let reset = match admission {
            Admission::Accept { reset, .. } | Admission::Discard { reset, .. } => reset,
        };
        if reset {
            self.discontinuity();
        }
        let mut decoded = Block {
            generation,
            first_sample: block.first_sample,
            time_ticks: block.time_ticks,
            admission,
            power_dbfs: None,
            access: Vec::new(),
            traffic: Vec::new(),
            shortened: Vec::new(),
        };
        if let Admission::Accept { segment, .. } = admission {
            let iq = &samples[..block.samples_read];
            if !iq.is_empty() {
                let power = iq
                    .iter()
                    .map(|value| f64::from(value.norm_sqr()))
                    .sum::<f64>()
                    / iq.len() as f64;
                decoded.power_dbfs = (power > 0.0).then(|| 10.0 * power.log10());
            }
            self.symbols.clear();
            self.symbols.extend(iq.iter().map(|value| Symbol {
                i: f64::from(value.re),
                q: f64::from(value.im),
            }));
            self.filtered.resize(iq.len(), Symbol { i: 0.0, q: 0.0 });
            self.filter.process(&self.symbols, &mut self.filtered);
            let rx_tick = |first_sample: f64| {
                block.time_ticks as f64
                    + (first_sample - block.first_sample as f64) * self.clocks.rx_tick_rate_hz
                        / self.clocks.rx_sample_rate_hz
            };
            let mut bursts = Vec::with_capacity(self.streams.max_bursts);
            self.streams.control.push_filtered(
                segment,
                block.first_sample,
                &self.symbols,
                &self.filtered,
                &mut bursts,
            )?;
            decoded.access = bursts
                .drain(..)
                .map(|burst| Decoded {
                    rx_tick: rx_tick(burst.first_sample),
                    burst,
                })
                .collect();
            if self.shared.shortened.load(Ordering::Acquire) {
                let mut bursts = Vec::with_capacity(self.streams.max_bursts);
                self.streams.shortened.push_filtered(
                    segment,
                    block.first_sample,
                    &self.symbols,
                    &self.filtered,
                    &mut bursts,
                )?;
                decoded.shortened = bursts
                    .into_iter()
                    .map(|burst| Decoded {
                        rx_tick: rx_tick(burst.first_sample),
                        burst,
                    })
                    .collect();
            } else {
                self.streams.shortened.discontinuity();
            }
            if self.shared.traffic.load(Ordering::Acquire) {
                let mut bursts = Vec::with_capacity(self.streams.max_bursts);
                self.streams.traffic.push_filtered(
                    segment,
                    block.first_sample,
                    &self.symbols,
                    &self.filtered,
                    &mut bursts,
                )?;
                decoded.traffic = bursts
                    .into_iter()
                    .map(|burst| Decoded {
                        rx_tick: rx_tick(burst.first_sample),
                        burst,
                    })
                    .collect();
            } else {
                self.streams.traffic.discontinuity();
            }
        }
        Ok(decoded)
    }
}
