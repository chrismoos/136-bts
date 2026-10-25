use super::{proto, unix_ms};
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
};
use tracing::{
    Event, Subscriber,
    field::{Field, Visit},
};
use tracing_subscriber::{Layer, layer::Context};
const EVENT_CAPACITY: usize = 256;
struct Rows {
    unapplied: mpsc::Receiver<proto::Event>,
    rows: VecDeque<proto::Event>,
}
impl Rows {
    fn apply_recorded(&mut self) {
        while let Ok(event) = self.unapplied.try_recv() {
            if self.rows.len() == EVENT_CAPACITY {
                self.rows.pop_front();
            }
            self.rows.push_back(event);
        }
    }
}
pub(crate) struct Events {
    rows: Mutex<Rows>,
    // Writers never take the rows lock, so a console read cannot make them drop an event.
    recorded: mpsc::Sender<proto::Event>,
    sequence: AtomicU64,
}
impl Default for Events {
    fn default() -> Self {
        let (recorded, unapplied) = mpsc::channel();
        Self {
            rows: Mutex::new(Rows {
                unapplied,
                rows: VecDeque::new(),
            }),
            recorded,
            sequence: AtomicU64::new(0),
        }
    }
}
impl Events {
    /// Returns one page of events, newest first.
    pub(crate) fn query(
        &self,
        query: proto::EventQuery,
        limit: usize,
    ) -> Result<proto::EventPage, tonic::Status> {
        let level = proto::EventLevel::try_from(query.level)
            .map_err(|_| tonic::Status::invalid_argument("Unknown event level"))?;
        let search = query.search.to_lowercase();
        let mut rows = self
            .rows
            .lock()
            .map_err(|_| tonic::Status::internal("Event log unavailable"))?;
        rows.apply_recorded();
        let mut components: Vec<String> = rows
            .rows
            .iter()
            .map(|event| event.component.clone())
            .collect();
        components.sort_unstable();
        components.dedup();
        let matching: Vec<_> = rows
            .rows
            .iter()
            .rev()
            .filter(|event| {
                (level == proto::EventLevel::Unspecified || event.level == query.level)
                    && (query.component.is_empty() || event.component == query.component)
                    && query
                        .before_sequence
                        .is_none_or(|sequence| event.sequence < sequence)
                    && (search.is_empty()
                        || format!("{} {}", event.message, event.fields)
                            .to_lowercase()
                            .contains(&search))
            })
            .collect();
        Ok(proto::EventPage {
            total: matching.len() as u32,
            events: matching
                .into_iter()
                .skip(query.offset as usize)
                .take(limit)
                .cloned()
                .collect(),
            components,
        })
    }
}
pub struct EventLayer(pub(crate) Arc<Events>);
#[derive(Default)]
struct Fields {
    message: String,
    values: Vec<String>,
}
impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = format!("{value:?}");
        } else {
            self.values.push(format!("{}={value:?}", field.name()));
        }
    }
}
impl<S: Subscriber> Layer<S> for EventLayer {
    fn on_event(&self, event: &Event<'_>, _: Context<'_, S>) {
        if *event.metadata().level() > tracing::Level::INFO {
            return;
        }
        let mut fields = Fields::default();
        event.record(&mut fields);
        let sequence = self.0.sequence.fetch_add(1, Ordering::Relaxed) + 1;
        let _ = self.0.recorded.send(proto::Event {
            sequence,
            unix_ms: unix_ms(),
            level: match *event.metadata().level() {
                tracing::Level::TRACE => proto::EventLevel::Trace,
                tracing::Level::DEBUG => proto::EventLevel::Debug,
                tracing::Level::INFO => proto::EventLevel::Info,
                tracing::Level::WARN => proto::EventLevel::Warn,
                tracing::Level::ERROR => proto::EventLevel::Error,
            } as i32,
            component: event
                .metadata()
                .target()
                .rsplit("::")
                .next()
                .unwrap_or_default()
                .replace('_', " "),
            message: fields.message,
            fields: fields.values.join(" "),
        });
        if let Ok(mut rows) = self.0.rows.try_lock() {
            rows.apply_recorded();
        }
    }
}
