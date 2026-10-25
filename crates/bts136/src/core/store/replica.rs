//! In-memory working copy of the state database with ordered write-behind to the file.
//!
//! The serving thread reads and writes only the in-memory copy. An update hook marks each
//! changed row, and `flush` copies the current contents of those rows to a store thread,
//! which commits them to the file in order. Replication copies row state rather than
//! statements, so a change that was later rolled back replicates as a no-op.

use super::{StateStore, StoreError};
use rusqlite::{Connection, MAIN_DB, TransactionBehavior, hooks::Action, types::Value};
use std::{
    collections::{BTreeSet, HashMap},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

const BATCH_QUEUE_DEPTH: usize = 64;
const RETRY_DELAY: Duration = Duration::from_millis(100);
const DRAIN_POLL: Duration = Duration::from_millis(1);
const SEQUENCE_TABLE: &str = "sqlite_sequence";
const MAIN_DATABASE: &str = "main";

type RowKey = (String, i64);

struct Batch {
    through_change: u64,
    deletes: Vec<RowKey>,
    upserts: Vec<(String, i64, Vec<Value>)>,
    sequence: Vec<(String, i64)>,
}

pub(super) struct Replication {
    dirty: Arc<Mutex<BTreeSet<RowKey>>>,
    changes: Arc<AtomicU64>,
    durable: Arc<AtomicU64>,
    healthy: Arc<AtomicBool>,
    batches: Option<mpsc::SyncSender<Batch>>,
    worker: Option<thread::JoinHandle<()>>,
}

impl std::fmt::Debug for Replication {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Replication")
            .field("changes", &self.changes.load(Ordering::Relaxed))
            .field("durable", &self.durable.load(Ordering::Relaxed))
            .finish()
    }
}

impl Drop for Replication {
    fn drop(&mut self) {
        self.batches.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl StateStore {
    pub fn open_replicated(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        let file = Self::open(path)?.connection;
        let mut memory = Connection::open_in_memory()?;
        memory.restore(MAIN_DB, path, None::<fn(rusqlite::backup::Progress)>)?;
        memory.pragma_update(None, "foreign_keys", true)?;

        let dirty = Arc::new(Mutex::new(BTreeSet::new()));
        let changes = Arc::new(AtomicU64::new(0));
        let hook_dirty = Arc::clone(&dirty);
        let hook_changes = Arc::clone(&changes);
        memory.update_hook(Some(
            move |_action: Action, database: &str, table: &str, rowid: i64| {
                if database != MAIN_DATABASE {
                    return;
                }
                hook_dirty
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert((table.to_owned(), rowid));
                hook_changes.fetch_add(1, Ordering::Release);
            },
        ))?;

        let durable = Arc::new(AtomicU64::new(0));
        let healthy = Arc::new(AtomicBool::new(true));
        let (batches, received) = mpsc::sync_channel(BATCH_QUEUE_DEPTH);
        let worker_durable = Arc::clone(&durable);
        let worker_healthy = Arc::clone(&healthy);
        let worker = thread::Builder::new()
            .name("136bts-store".into())
            .spawn(move || persist(file, received, &worker_durable, &worker_healthy))
            .map_err(|_| StoreError::CorruptState)?;

        Ok(Self {
            connection: memory,
            replication: Some(Replication {
                dirty,
                changes,
                durable,
                healthy,
                batches: Some(batches),
                worker: Some(worker),
            }),
        })
    }

    /// Sends every row changed since the last flush to the store thread. Does nothing
    /// inside an open transaction or when the store thread's queue is full, so the rows
    /// stay marked and go out with a later flush.
    pub fn flush(&mut self) -> Result<(), StoreError> {
        let Some(replication) = &self.replication else {
            return Ok(());
        };
        if !self.connection.is_autocommit() {
            return Ok(());
        }
        let through_change = replication.changes.load(Ordering::Acquire);
        let rows = std::mem::take(
            &mut *replication
                .dirty
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()),
        );
        if rows.is_empty() {
            return Ok(());
        }
        let mut batch = Batch {
            through_change,
            deletes: Vec::new(),
            upserts: Vec::new(),
            sequence: Vec::new(),
        };
        for (table, rowid) in &rows {
            if table == SEQUENCE_TABLE {
                continue;
            }
            let mut statement = self
                .connection
                .prepare_cached(&format!("SELECT * FROM \"{table}\" WHERE rowid=?1"))?;
            let width = statement.column_count();
            let mut found = statement.query([rowid])?;
            match found.next()? {
                Some(row) => {
                    let values = (0..width)
                        .map(|index| row.get::<_, Value>(index))
                        .collect::<Result<Vec<_>, _>>()?;
                    batch.upserts.push((table.clone(), *rowid, values));
                }
                None => batch.deletes.push((table.clone(), *rowid)),
            }
        }
        let mut statement = self
            .connection
            .prepare_cached("SELECT name,seq FROM sqlite_sequence")?;
        batch.sequence = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let Some(sender) = &replication.batches else {
            return Ok(());
        };
        if let Err(mpsc::TrySendError::Full(_) | mpsc::TrySendError::Disconnected(_)) =
            sender.try_send(batch)
        {
            replication
                .dirty
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .extend(rows);
        }
        Ok(())
    }

    pub fn is_replicated(&self) -> bool {
        self.replication.is_some()
    }

    /// Marks the changes made so far. Work that must not be visible to a handset until
    /// those changes are on disk waits for `is_durable` on this mark.
    pub fn change_mark(&self) -> u64 {
        self.replication
            .as_ref()
            .map_or(0, |replication| replication.changes.load(Ordering::Acquire))
    }

    pub fn is_durable(&self, mark: u64) -> bool {
        self.replication
            .as_ref()
            .is_none_or(|replication| replication.durable.load(Ordering::Acquire) >= mark)
    }

    pub fn undurable_changes(&self) -> u64 {
        self.replication.as_ref().map_or(0, |replication| {
            replication
                .changes
                .load(Ordering::Acquire)
                .saturating_sub(replication.durable.load(Ordering::Acquire))
        })
    }

    /// False while the store thread is retrying a failed write to the file.
    pub fn persistence_healthy(&self) -> bool {
        self.replication
            .as_ref()
            .is_none_or(|replication| replication.healthy.load(Ordering::Acquire))
    }

    pub fn drain(&mut self, timeout: Duration) -> Result<bool, StoreError> {
        let deadline = Instant::now() + timeout;
        let mark = self.change_mark();
        loop {
            self.flush()?;
            if self.is_durable(mark) {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            thread::sleep(DRAIN_POLL);
        }
    }
}

fn persist(
    mut file: Connection,
    batches: mpsc::Receiver<Batch>,
    durable: &AtomicU64,
    healthy: &AtomicBool,
) {
    if let Err(error) = file.pragma_update(None, "foreign_keys", false) {
        tracing::error!(%error, "state store thread could not prepare the database file");
        healthy.store(false, Ordering::Release);
        return;
    }
    let mut columns = HashMap::new();
    while let Ok(batch) = batches.recv() {
        loop {
            match apply(&mut file, &mut columns, &batch) {
                Ok(()) => {
                    healthy.store(true, Ordering::Release);
                    durable.fetch_max(batch.through_change, Ordering::AcqRel);
                    break;
                }
                Err(error) => {
                    if healthy.swap(false, Ordering::AcqRel) {
                        tracing::error!(%error, "state write to the database file failed, retrying");
                    }
                    thread::sleep(RETRY_DELAY);
                }
            }
        }
    }
}

fn apply(
    file: &mut Connection,
    columns: &mut HashMap<String, String>,
    batch: &Batch,
) -> rusqlite::Result<()> {
    let transaction = file.transaction_with_behavior(TransactionBehavior::Immediate)?;
    for (table, rowid) in &batch.deletes {
        transaction
            .prepare_cached(&format!("DELETE FROM \"{table}\" WHERE rowid=?1"))?
            .execute([rowid])?;
    }
    for (table, rowid, values) in &batch.upserts {
        if !columns.contains_key(table) {
            let names = transaction
                .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))?
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            let list = names
                .iter()
                .map(|name| format!("\"{name}\""))
                .collect::<Vec<_>>()
                .join(",");
            let placeholders = (2..=names.len() + 1)
                .map(|index| format!("?{index}"))
                .collect::<Vec<_>>()
                .join(",");
            columns.insert(
                table.clone(),
                format!(
                    "INSERT OR REPLACE INTO \"{table}\"(rowid,{list}) VALUES(?1,{placeholders})"
                ),
            );
        }
        let mut parameters = Vec::with_capacity(values.len() + 1);
        parameters.push(Value::Integer(*rowid));
        parameters.extend(values.iter().cloned());
        transaction
            .prepare_cached(&columns[table])?
            .execute(rusqlite::params_from_iter(parameters))?;
    }
    transaction.execute("DELETE FROM sqlite_sequence", [])?;
    for (name, sequence) in &batch.sequence {
        transaction.execute(
            "INSERT INTO sqlite_sequence(name,seq) VALUES(?1,?2)",
            rusqlite::params![name, sequence],
        )?;
    }
    transaction.commit()
}

#[cfg(test)]
pub(crate) type TableRows = Vec<(String, Vec<Vec<Value>>)>;

#[cfg(test)]
pub(crate) fn table_rows(connection: &Connection) -> TableRows {
    let tables = connection
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    tables
        .into_iter()
        .map(|table| {
            let query = if table == SEQUENCE_TABLE {
                "SELECT name,seq FROM sqlite_sequence ORDER BY name".to_owned()
            } else {
                format!("SELECT rowid,* FROM \"{table}\" ORDER BY rowid")
            };
            let mut statement = connection.prepare(&query).unwrap();
            let width = statement.column_count();
            let rows = statement
                .query_map([], |row| {
                    (0..width).map(|index| row.get::<_, Value>(index)).collect()
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            (table, rows)
        })
        .collect()
}

#[cfg(test)]
impl StateStore {
    pub(crate) fn rows(&self) -> TableRows {
        table_rows(&self.connection)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    static SEQUENCE: AtomicUsize = AtomicUsize::new(0);
    const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

    pub(in crate::core::store) struct TempFile(pub std::path::PathBuf);
    impl TempFile {
        pub fn new() -> Self {
            Self(std::env::temp_dir().join(format!(
                "136bts-replica-{}-{}.sqlite3",
                std::process::id(),
                SEQUENCE.fetch_add(1, Ordering::Relaxed)
            )))
        }
    }
    impl Drop for TempFile {
        fn drop(&mut self) {
            for suffix in ["", "-wal", "-shm"] {
                let _ = std::fs::remove_file(format!("{}{suffix}", self.0.display()));
            }
        }
    }

    fn file_rows(path: &Path) -> TableRows {
        table_rows(&Connection::open(path).unwrap())
    }

    #[test]
    fn inserts_updates_deletes_and_rollbacks_reach_the_file() {
        let file = TempFile::new();
        let mut store = StateStore::open_replicated(&file.0).unwrap();
        store
            .connection
            .execute(
                "INSERT OR REPLACE INTO provisioning_state(id,seeded) VALUES(1,1)",
                [],
            )
            .unwrap();
        assert!(store.change_mark() > 0);
        assert!(store.drain(DRAIN_TIMEOUT).unwrap());
        assert_eq!(file_rows(&file.0), store.rows());

        {
            let transaction = store.connection.transaction().unwrap();
            transaction
                .execute("UPDATE provisioning_state SET seeded=0 WHERE id=1", [])
                .unwrap();
            transaction.rollback().unwrap();
        }
        assert!(store.drain(DRAIN_TIMEOUT).unwrap());
        assert_eq!(file_rows(&file.0), store.rows());

        store
            .connection
            .execute("DELETE FROM provisioning_state WHERE id=1", [])
            .unwrap();
        store
            .connection
            .execute_batch("CREATE TEMP TABLE scratch(value); INSERT INTO scratch VALUES(1);")
            .unwrap();
        assert!(store.drain(DRAIN_TIMEOUT).unwrap());
        assert_eq!(file_rows(&file.0), store.rows());
    }

    #[test]
    fn a_reopened_store_starts_from_the_replicated_file() {
        let file = TempFile::new();
        let mut store = StateStore::open_replicated(&file.0).unwrap();
        store
            .connection
            .execute(
                "INSERT OR REPLACE INTO provisioning_state(id,seeded) VALUES(1,1)",
                [],
            )
            .unwrap();
        assert!(store.drain(DRAIN_TIMEOUT).unwrap());
        let expected = store.rows();
        drop(store);
        let reopened = StateStore::open_replicated(&file.0).unwrap();
        assert_eq!(reopened.rows(), expected);
        assert_eq!(reopened.change_mark(), 0);
    }

    #[test]
    fn changes_are_not_durable_until_flushed() {
        let file = TempFile::new();
        let mut store = StateStore::open_replicated(&file.0).unwrap();
        store
            .connection
            .execute(
                "INSERT OR REPLACE INTO provisioning_state(id,seeded) VALUES(1,1)",
                [],
            )
            .unwrap();
        let mark = store.change_mark();
        assert!(!store.is_durable(mark));
        assert!(store.drain(DRAIN_TIMEOUT).unwrap());
        assert!(store.is_durable(mark));
        assert_eq!(store.undurable_changes(), 0);
    }
}
