//! SQLite event journal — durable, crash-safe persistence of every engine event.
//!
//! The journal is the universal sink for all `EngineEvent` variants emitted across the
//! system (trade events, order state changes, circuit breakers, regime transitions, and
//! generic system events). Producers send events through a bounded crossbeam channel
//! using non-blocking `try_send()` — the hot path never waits on disk I/O.
//!
//! The dedicated journal worker drains the channel on a single thread (rusqlite's
//! `Connection` is `!Send`) and routes each event to its corresponding SQLite table.
//! Writes are batched (up to 64 events or 100 ms) inside a transaction to amortize
//! fsync cost, and a passive WAL checkpoint runs every 60 seconds to bound WAL growth.
//!
//! Crash safety: WAL mode + `synchronous=NORMAL` guarantees that committed transactions
//! survive process crashes. The last few milliseconds of in-flight events may be lost,
//! which is acceptable per NFR9. Every trade-related event carries a `decision_id` to
//! enable end-to-end causality tracing (NFR17).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError, TrySendError};
use futures_bmad_core::{FixedPrice, Side, TradeSource, UnixNanos};
use rusqlite::{Connection, params};
use tracing::{debug, error, info, warn};

/// Bounded channel capacity from the architecture spec.
pub const JOURNAL_CHANNEL_CAPACITY: usize = 8192;

/// Batch size for journal writes — flush after this many events.
pub const BATCH_SIZE: usize = 64;

/// Maximum duration to accumulate events before flushing a partial batch.
pub const BATCH_INTERVAL: Duration = Duration::from_millis(100);

/// Period between passive WAL checkpoint operations.
pub const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(60);

/// Default location of the journal database, relative to the working directory.
pub const DEFAULT_DB_PATH: &str = "data/journal.db";

/// SQL definitions for journal tables.
///
/// Storage conventions (architecture, "SQLite Naming"):
/// - timestamps as INTEGER nanoseconds
/// - prices as INTEGER quarter-ticks (FixedPrice raw)
/// - table names snake_case plural
///
/// Story 7.4 — `source` column on every trade-related table tags the row
/// origin (`live`, `paper`, `replay`). Defaults to `'live'` so existing
/// journals (created before the column was introduced) continue to read as
/// live data after the migration runs in [`EventJournal::new`].
const CREATE_TRADE_EVENTS: &str = "
    CREATE TABLE IF NOT EXISTS trade_events (
        id           INTEGER PRIMARY KEY AUTOINCREMENT,
        timestamp    INTEGER NOT NULL,
        decision_id  INTEGER NOT NULL,
        order_id     INTEGER,
        symbol_id    INTEGER NOT NULL,
        side         INTEGER NOT NULL,
        price        INTEGER NOT NULL,
        size         INTEGER NOT NULL,
        kind         TEXT    NOT NULL,
        source       TEXT    NOT NULL DEFAULT 'live'
    );
";

const CREATE_ORDER_STATES: &str = "
    CREATE TABLE IF NOT EXISTS order_states (
        id           INTEGER PRIMARY KEY AUTOINCREMENT,
        timestamp    INTEGER NOT NULL,
        order_id     INTEGER NOT NULL,
        decision_id  INTEGER,
        from_state   TEXT    NOT NULL,
        to_state     TEXT    NOT NULL,
        source       TEXT    NOT NULL DEFAULT 'live'
    );
";

/// Index on `source` for efficient `WHERE source = ?` filtering used by
/// [`crate::persistence::query::JournalQuery`] (Story 7.4 Task 1.5).
const CREATE_SOURCE_INDEXES: &str = "
    CREATE INDEX IF NOT EXISTS idx_trade_events_source ON trade_events(source);
    CREATE INDEX IF NOT EXISTS idx_order_states_source ON order_states(source);
";

/// Migration applied at [`EventJournal::new`] to add the `source` column to
/// journals created before Story 7.4. Idempotent — guarded by a runtime
/// schema check so a fresh DB (already has the column) skips the ALTER.
fn migrate_add_source_column(conn: &Connection) -> Result<(), JournalError> {
    for table in ["trade_events", "order_states"] {
        if !column_exists(conn, table, "source")? {
            let stmt =
                format!("ALTER TABLE {table} ADD COLUMN source TEXT NOT NULL DEFAULT 'live'");
            conn.execute(&stmt, [])?;
            info!(target: "journal", table, "added source column (Story 7.4 migration)");
        }
    }
    Ok(())
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool, JournalError> {
    // PRAGMA table_info returns one row per column; matching on `name` (col 1)
    // is the canonical SQLite check for "does this column exist?".
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

const CREATE_CIRCUIT_BREAKER_EVENTS: &str = "
    CREATE TABLE IF NOT EXISTS circuit_breaker_events (
        id           INTEGER PRIMARY KEY AUTOINCREMENT,
        timestamp    INTEGER NOT NULL,
        breaker_type TEXT    NOT NULL,
        triggered    INTEGER NOT NULL,
        reason       TEXT    NOT NULL
    );
";

const CREATE_REGIME_TRANSITIONS: &str = "
    CREATE TABLE IF NOT EXISTS regime_transitions (
        id            INTEGER PRIMARY KEY AUTOINCREMENT,
        timestamp     INTEGER NOT NULL,
        from_regime   TEXT    NOT NULL,
        to_regime     TEXT    NOT NULL
    );
";

const CREATE_SYSTEM_EVENTS: &str = "
    CREATE TABLE IF NOT EXISTS system_events (
        id           INTEGER PRIMARY KEY AUTOINCREMENT,
        timestamp    INTEGER NOT NULL,
        category     TEXT    NOT NULL,
        message      TEXT    NOT NULL
    );
";

const ALL_TABLE_DDL: &[&str] = &[
    CREATE_TRADE_EVENTS,
    CREATE_ORDER_STATES,
    CREATE_CIRCUIT_BREAKER_EVENTS,
    CREATE_REGIME_TRANSITIONS,
    CREATE_SYSTEM_EVENTS,
];

// ---------------------------------------------------------------------------
// Event payload records
// ---------------------------------------------------------------------------

/// Trade-related event recorded into `trade_events`.
///
/// Trade events MUST carry a `decision_id` so every fill/order-related entry chains back
/// to the originating signal evaluation (NFR17).
///
/// Story 7.4 — `source` distinguishes paper / replay / live entries. Set
/// implicitly by the [`JournalSender`] at send time; callers only need to
/// override `source` when constructing a record outside the normal sender
/// flow (tests, direct `EventJournal::write_event`).
#[derive(Debug, Clone)]
pub struct TradeEventRecord {
    pub timestamp: UnixNanos,
    /// Required — every trade event must reference its originating decision.
    pub decision_id: Option<u64>,
    pub order_id: Option<u64>,
    pub symbol_id: u32,
    pub side: Side,
    pub price: FixedPrice,
    pub size: u32,
    /// Free-form classification: "submit", "fill", "partial_fill", "cancel", etc.
    pub kind: String,
    /// Origin of the trade entry — `Live` by default, overridden to `Paper` /
    /// `Replay` by the orchestrator that owns the [`JournalSender`].
    pub source: TradeSource,
}

/// Order state machine transition recorded into `order_states`.
///
/// Story 7.4 — `source` mirrors the same tag scheme as [`TradeEventRecord`].
#[derive(Debug, Clone)]
pub struct OrderStateChangeRecord {
    pub timestamp: UnixNanos,
    pub order_id: u64,
    pub decision_id: Option<u64>,
    pub from_state: String,
    pub to_state: String,
    /// Origin of the state transition — see [`TradeEventRecord::source`].
    pub source: TradeSource,
}

/// Circuit breaker activation recorded into `circuit_breaker_events`.
#[derive(Debug, Clone)]
pub struct CircuitBreakerEventRecord {
    pub timestamp: UnixNanos,
    pub breaker_type: String,
    pub triggered: bool,
    pub reason: String,
}

/// Market regime transition recorded into `regime_transitions`.
#[derive(Debug, Clone)]
pub struct RegimeTransitionRecord {
    pub timestamp: UnixNanos,
    pub from_regime: String,
    pub to_regime: String,
}

/// Generic system event recorded into `system_events`.
#[derive(Debug, Clone)]
pub struct SystemEventRecord {
    pub timestamp: UnixNanos,
    pub category: String,
    pub message: String,
}

// ---------------------------------------------------------------------------
// EngineEvent — the journal sink discriminator
// ---------------------------------------------------------------------------

/// Journal-side event variant — discriminates which table a payload is routed to.
///
/// This is intentionally separate from `core::EngineEvent` (which is the in-memory
/// engine-bus enumeration). The journal owns its own variant set keyed on its table
/// schema so the persistence layer can evolve independently of the in-memory event
/// bus shape.
#[derive(Debug, Clone)]
pub enum EngineEvent {
    TradeEvent(TradeEventRecord),
    OrderStateChange(OrderStateChangeRecord),
    CircuitBreakerEvent(CircuitBreakerEventRecord),
    RegimeTransition(RegimeTransitionRecord),
    SystemEvent(SystemEventRecord),
}

impl EngineEvent {
    /// Top-level event timestamp (used for batching diagnostics).
    pub fn timestamp(&self) -> UnixNanos {
        match self {
            EngineEvent::TradeEvent(r) => r.timestamp,
            EngineEvent::OrderStateChange(r) => r.timestamp,
            EngineEvent::CircuitBreakerEvent(r) => r.timestamp,
            EngineEvent::RegimeTransition(r) => r.timestamp,
            EngineEvent::SystemEvent(r) => r.timestamp,
        }
    }

    /// Required `decision_id` for trade events; surfaces missing causal chain as `None`.
    pub fn decision_id(&self) -> Option<u64> {
        match self {
            EngineEvent::TradeEvent(r) => r.decision_id,
            EngineEvent::OrderStateChange(r) => r.decision_id,
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

// ---------------------------------------------------------------------------
// Channel wrappers
// ---------------------------------------------------------------------------

/// Multiproducer sender for journal events.
///
/// Uses non-blocking `try_send()`: if the channel is full the event is dropped and
/// a warning emitted. The hot path must NEVER block on the journal — losing a small
/// number of audit events under extreme backpressure is preferable to stalling the
/// event loop.
///
/// Story 7.4 — every sender carries an optional `source_override`. When
/// `Some`, the sender stamps the override onto every trade/order record at
/// send time, replacing whatever the caller put on the record. This is the
/// single seam through which paper / replay orchestrators tag their entries
/// without requiring every call site to know which mode it is running in.
/// The default sender (from [`EventJournal::channel`]) has no override so
/// records keep whatever value the caller set (defaulting to
/// [`TradeSource::Live`]).
#[derive(Clone)]
pub struct JournalSender {
    inner: Sender<EngineEvent>,
    source_override: Option<TradeSource>,
}

impl JournalSender {
    /// Attempt to enqueue an event. On full channel, log a warning and drop.
    ///
    /// Returns `true` if the event was accepted, `false` if it was dropped or the
    /// receiver was disconnected.
    ///
    /// If a `source_override` is set on this sender, trade-related variants
    /// (`TradeEvent`, `OrderStateChange`) have their `source` field rewritten
    /// to the override value before being enqueued.
    pub fn send(&self, mut event: EngineEvent) -> bool {
        if let Some(src) = self.source_override {
            apply_source_override(&mut event, src);
        }
        match self.inner.try_send(event) {
            Ok(()) => true,
            Err(TrySendError::Full(dropped)) => {
                warn!(
                    target: "journal",
                    decision_id = ?dropped.decision_id(),
                    "journal channel full — dropping event (backpressure)"
                );
                false
            }
            Err(TrySendError::Disconnected(_)) => {
                error!(target: "journal", "journal channel disconnected — event dropped");
                false
            }
        }
    }

    /// Return a clone of this sender with `source_override = Some(source)`.
    /// Used by the paper / replay orchestrators (Story 7.4 Task 2) to tag
    /// every record they enqueue without modifying the existing call sites
    /// in the order manager and bracket manager. Composes cleanly so a
    /// JournalSender constructed with one source can be re-tagged downstream.
    pub fn with_source(&self, source: TradeSource) -> Self {
        Self {
            inner: self.inner.clone(),
            source_override: Some(source),
        }
    }

    /// Inspect the configured source override (`None` for the default
    /// live-tagged sender). Exposed so orchestrators can assert on the
    /// expected source at attach time.
    pub fn source_override(&self) -> Option<TradeSource> {
        self.source_override
    }

    /// Approximate current channel depth (for monitoring/tests).
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the channel is currently empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

fn apply_source_override(event: &mut EngineEvent, source: TradeSource) {
    match event {
        EngineEvent::TradeEvent(r) => r.source = source,
        EngineEvent::OrderStateChange(r) => r.source = source,
        // Other variants are mode-agnostic (circuit breakers, regime
        // transitions, system events) — they share schema across paper /
        // replay / live and do not carry a source column.
        EngineEvent::CircuitBreakerEvent(_)
        | EngineEvent::RegimeTransition(_)
        | EngineEvent::SystemEvent(_) => {}
    }
}

/// Receiver half — owned by the journal worker.
pub struct JournalReceiver {
    inner: Receiver<EngineEvent>,
}

impl JournalReceiver {
    /// Non-blocking try_recv exposing the full crossbeam result so callers can
    /// distinguish `Empty` (transient) from `Disconnected` (terminal).
    fn try_recv(&self) -> Result<EngineEvent, TryRecvError> {
        self.inner.try_recv()
    }

    /// Bounded blocking recv. `Timeout` is a transient signal (flush partial batch);
    /// `Disconnected` is the terminal signal (flush partial batch, then exit).
    fn recv_timeout(&self, timeout: Duration) -> Result<EngineEvent, RecvTimeoutError> {
        self.inner.recv_timeout(timeout)
    }

    /// Test-only helper: non-blocking receive returning `None` for both `Empty`
    /// and `Disconnected`. Used by sibling crate tests (e.g. `order_manager`)
    /// that need to inspect dispatched events without spinning up the worker.
    pub fn try_recv_for_test(&self) -> Option<EngineEvent> {
        self.inner.try_recv().ok()
    }
}

// ---------------------------------------------------------------------------
// EventJournal
// ---------------------------------------------------------------------------

/// Owns the SQLite connection and exposes the channel + worker loop.
///
/// `EventJournal` is `!Send` because it holds a `rusqlite::Connection`. Callers should
/// instantiate it on the dedicated journal thread (or inside `tokio::task::spawn_blocking`)
/// and pin it there for its lifetime.
pub struct EventJournal {
    conn: Connection,
    db_path: PathBuf,
}

impl EventJournal {
    /// Open or create the journal database at `db_path` in WAL mode.
    pub fn new(db_path: &Path) -> Result<Self, JournalError> {
        if let Some(parent) = db_path.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(db_path)?;

        // WAL mode: required for concurrent reader/writer + passive checkpoints.
        // PRAGMA journal_mode returns the mode string — verify it took.
        let mode: String = conn.query_row("PRAGMA journal_mode = WAL;", [], |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(JournalError::Sqlite(rusqlite::Error::InvalidQuery));
        }

        // synchronous = NORMAL: WAL-safe; commits durable on transaction boundary,
        // last few ms may be lost on OS crash (NFR9 acceptable).
        conn.pragma_update(None, "synchronous", "NORMAL")?;

        // busy_timeout: 5s — give passive checkpoints a window to back off if a
        // concurrent reader is mid-query.
        conn.pragma_update(None, "busy_timeout", 5_000)?;

        // Create all journal tables in a single transaction.
        let tx = conn.unchecked_transaction()?;
        for ddl in ALL_TABLE_DDL {
            tx.execute_batch(ddl)?;
        }
        tx.commit()?;

        // Story 7.4 Task 1 — backfill source column on existing journals
        // (no-op on fresh DBs where the column already exists from the
        // CREATE TABLE statement above) and create the source-column
        // indexes for efficient WHERE source = ? filtering.
        migrate_add_source_column(&conn)?;
        conn.execute_batch(CREATE_SOURCE_INDEXES)?;

        info!(target: "journal", path = %db_path.display(), "event journal opened");

        Ok(Self {
            conn,
            db_path: db_path.to_path_buf(),
        })
    }

    /// Convenience: open the default journal at `data/journal.db`.
    pub fn open_default() -> Result<Self, JournalError> {
        Self::new(Path::new(DEFAULT_DB_PATH))
    }

    /// Construct a fresh bounded channel pair sized to `JOURNAL_CHANNEL_CAPACITY`.
    ///
    /// The returned [`JournalSender`] has no source override — records are
    /// recorded with whatever `source` the caller put on them (defaulting to
    /// [`TradeSource::Live`]). For paper / replay use
    /// [`Self::channel_with_source`] or call [`JournalSender::with_source`]
    /// on the returned sender.
    pub fn channel() -> (JournalSender, JournalReceiver) {
        let (tx, rx) = crossbeam_channel::bounded(JOURNAL_CHANNEL_CAPACITY);
        (
            JournalSender {
                inner: tx,
                source_override: None,
            },
            JournalReceiver { inner: rx },
        )
    }

    /// Story 7.4 — construct a channel whose sender stamps every trade /
    /// order record with `source`. Equivalent to
    /// `EventJournal::channel().0.with_source(source)` plus the matching
    /// receiver, packaged as a single call for the orchestrator wiring path.
    pub fn channel_with_source(source: TradeSource) -> (JournalSender, JournalReceiver) {
        let (tx, rx) = crossbeam_channel::bounded(JOURNAL_CHANNEL_CAPACITY);
        (
            JournalSender {
                inner: tx,
                source_override: Some(source),
            },
            JournalReceiver { inner: rx },
        )
    }

    /// Run the synchronous batching write loop until all senders disconnect.
    ///
    /// Drains events from `receiver`, accumulating up to `BATCH_SIZE` or
    /// `BATCH_INTERVAL`, whichever comes first, then flushes them in a single
    /// transaction. Periodically issues a passive WAL checkpoint.
    ///
    /// Termination is driven solely by `RecvTimeoutError::Disconnected` — i.e., all
    /// senders dropped AND the channel fully drained. A transient empty queue
    /// (`Timeout`) only triggers a partial-batch flush; the loop never exits while
    /// any sender is still alive.
    ///
    /// This function blocks the calling thread; in async contexts spawn it via
    /// `tokio::task::spawn_blocking`.
    pub fn run(&mut self, receiver: JournalReceiver) -> Result<(), JournalError> {
        let mut batch: Vec<EngineEvent> = Vec::with_capacity(BATCH_SIZE);
        let mut last_flush = Instant::now();
        let mut last_checkpoint = Instant::now();
        // Set on TryRecvError::Disconnected encountered during the inner drain — we
        // finish flushing the current batch before exiting so no event is lost.
        let mut disconnected = false;

        loop {
            // Block up to the remaining batch window for the next event. Using
            // recv_timeout (rather than try_recv + sleep) is essential: every event
            // received here is then either pushed onto the batch or is the terminal
            // Disconnected signal — there is NO path that consumes-and-discards.
            let wait = BATCH_INTERVAL.saturating_sub(last_flush.elapsed());
            match receiver.recv_timeout(wait) {
                Ok(event) => {
                    batch.push(event);
                    // Drain any other immediately-available events into the batch.
                    while batch.len() < BATCH_SIZE {
                        match receiver.try_recv() {
                            Ok(more) => batch.push(more),
                            Err(TryRecvError::Empty) => break,
                            Err(TryRecvError::Disconnected) => {
                                // All senders gone — flush the batch we just built,
                                // then exit cleanly via the outer break.
                                disconnected = true;
                                break;
                            }
                        }
                    }
                }
                Err(RecvTimeoutError::Timeout) => {
                    // Transient empty queue — fall through to flush whatever we have
                    // (which may be nothing). Do NOT exit; senders may still be alive.
                }
                Err(RecvTimeoutError::Disconnected) => {
                    // All senders dropped AND channel drained — terminal signal.
                    disconnected = true;
                }
            }

            let interval_elapsed = last_flush.elapsed() >= BATCH_INTERVAL;
            let batch_full = batch.len() >= BATCH_SIZE;
            if !batch.is_empty() && (batch_full || interval_elapsed || disconnected) {
                self.flush_batch(&mut batch)?;
                last_flush = Instant::now();
            } else if batch.is_empty() && interval_elapsed {
                // Reset the batch-window timer so the next loop iteration's
                // recv_timeout uses the full window, not a saturated zero wait.
                last_flush = Instant::now();
            }

            // Periodic passive WAL checkpoint.
            if last_checkpoint.elapsed() >= CHECKPOINT_INTERVAL {
                if let Err(e) = self.checkpoint() {
                    warn!(target: "journal", error = %e, "wal checkpoint failed");
                }
                last_checkpoint = Instant::now();
            }

            if disconnected {
                debug!(target: "journal", "all senders disconnected, journal worker exiting");
                break;
            }
        }

        // Defensive: flush anything still buffered (the disconnected branch above
        // already flushes when batch is non-empty, but cover the edge case).
        if !batch.is_empty() {
            self.flush_batch(&mut batch)?;
        }

        Ok(())
    }

    /// Persist a single event outside the run loop (used by tests).
    pub fn write_event(&mut self, event: &EngineEvent) -> Result<(), JournalError> {
        let tx = self.conn.unchecked_transaction()?;
        Self::write_one(&tx, event)?;
        tx.commit()?;
        Ok(())
    }

    /// Run a passive WAL checkpoint. Never blocks active writers.
    pub fn checkpoint(&mut self) -> Result<(i64, i64), JournalError> {
        // wal_checkpoint(PASSIVE) returns 3 columns: busy(0/1), log frames, checkpointed.
        let (log, ckpt): (i64, i64) =
            self.conn
                .query_row("PRAGMA wal_checkpoint(PASSIVE);", [], |row| {
                    Ok((row.get::<_, i64>(1)?, row.get::<_, i64>(2)?))
                })?;
        debug!(
            target: "journal",
            wal_pages = log,
            checkpointed = ckpt,
            "wal checkpoint complete"
        );
        Ok((log, ckpt))
    }

    /// The underlying database path (informational).
    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    fn flush_batch(&mut self, batch: &mut Vec<EngineEvent>) -> Result<(), JournalError> {
        let tx = self.conn.unchecked_transaction()?;
        for event in batch.iter() {
            Self::write_one(&tx, event)?;
        }
        tx.commit()?;
        debug!(target: "journal", count = batch.len(), "flushed batch");
        batch.clear();
        Ok(())
    }

    fn write_one(tx: &rusqlite::Transaction<'_>, event: &EngineEvent) -> Result<(), JournalError> {
        match event {
            EngineEvent::TradeEvent(r) => {
                if r.decision_id.is_none() {
                    error!(
                        target: "journal",
                        order_id = ?r.order_id,
                        kind = %r.kind,
                        "trade event missing decision_id (NFR17 violation)"
                    );
                }
                tx.execute(
                    "INSERT INTO trade_events
                        (timestamp, decision_id, order_id, symbol_id, side, price, size, kind, source)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                    params![
                        r.timestamp.as_nanos() as i64,
                        // Persist decision_id as 0 sentinel when missing — keeps NOT NULL.
                        r.decision_id.unwrap_or(0) as i64,
                        r.order_id.map(|id| id as i64),
                        r.symbol_id as i64,
                        side_to_i64(r.side),
                        r.price.raw(),
                        r.size as i64,
                        r.kind.as_str(),
                        r.source.as_str(),
                    ],
                )?;
            }
            EngineEvent::OrderStateChange(r) => {
                tx.execute(
                    "INSERT INTO order_states
                        (timestamp, order_id, decision_id, from_state, to_state, source)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        r.timestamp.as_nanos() as i64,
                        r.order_id as i64,
                        r.decision_id.map(|id| id as i64),
                        r.from_state.as_str(),
                        r.to_state.as_str(),
                        r.source.as_str(),
                    ],
                )?;
            }
            EngineEvent::CircuitBreakerEvent(r) => {
                tx.execute(
                    "INSERT INTO circuit_breaker_events
                        (timestamp, breaker_type, triggered, reason)
                     VALUES (?1, ?2, ?3, ?4)",
                    params![
                        r.timestamp.as_nanos() as i64,
                        r.breaker_type.as_str(),
                        r.triggered as i64,
                        r.reason.as_str(),
                    ],
                )?;
            }
            EngineEvent::RegimeTransition(r) => {
                tx.execute(
                    "INSERT INTO regime_transitions
                        (timestamp, from_regime, to_regime)
                     VALUES (?1, ?2, ?3)",
                    params![
                        r.timestamp.as_nanos() as i64,
                        r.from_regime.as_str(),
                        r.to_regime.as_str(),
                    ],
                )?;
            }
            EngineEvent::SystemEvent(r) => {
                tx.execute(
                    "INSERT INTO system_events
                        (timestamp, category, message)
                     VALUES (?1, ?2, ?3)",
                    params![
                        r.timestamp.as_nanos() as i64,
                        r.category.as_str(),
                        r.message.as_str(),
                    ],
                )?;
            }
        }
        Ok(())
    }
}

fn side_to_i64(side: Side) -> i64 {
    match side {
        Side::Buy => 0,
        Side::Sell => 1,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use tempfile::TempDir;

    fn make_journal() -> (EventJournal, TempDir) {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("journal.db");
        let journal = EventJournal::new(&path).expect("open journal");
        (journal, dir)
    }

    fn sample_trade(decision_id: Option<u64>) -> EngineEvent {
        EngineEvent::TradeEvent(TradeEventRecord {
            timestamp: UnixNanos::new(1_700_000_000_000_000_000),
            decision_id,
            order_id: Some(42),
            symbol_id: 1,
            side: Side::Buy,
            price: FixedPrice::new(17929), // 4482.25
            size: 1,
            kind: "submit".to_string(),
            source: TradeSource::Live,
        })
    }

    fn sample_order_state() -> EngineEvent {
        EngineEvent::OrderStateChange(OrderStateChangeRecord {
            timestamp: UnixNanos::new(1_700_000_000_000_000_001),
            order_id: 42,
            decision_id: Some(7),
            from_state: "Idle".to_string(),
            to_state: "Submitted".to_string(),
            source: TradeSource::Live,
        })
    }

    fn sample_breaker() -> EngineEvent {
        EngineEvent::CircuitBreakerEvent(CircuitBreakerEventRecord {
            timestamp: UnixNanos::new(1_700_000_000_000_000_002),
            breaker_type: "DailyLossLimit".to_string(),
            triggered: true,
            reason: "max daily loss exceeded".to_string(),
        })
    }

    fn sample_regime() -> EngineEvent {
        EngineEvent::RegimeTransition(RegimeTransitionRecord {
            timestamp: UnixNanos::new(1_700_000_000_000_000_003),
            from_regime: "Unknown".to_string(),
            to_regime: "Trending".to_string(),
        })
    }

    fn sample_system() -> EngineEvent {
        EngineEvent::SystemEvent(SystemEventRecord {
            timestamp: UnixNanos::new(1_700_000_000_000_000_004),
            category: "startup".to_string(),
            message: "engine online".to_string(),
        })
    }

    /// 6.1 — Initialization creates DB file, all tables, and WAL mode.
    #[test]
    fn initialization_creates_tables_in_wal_mode() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("journal.db");
        let journal = EventJournal::new(&path).expect("open");

        assert!(path.exists(), "db file should exist");

        let mode: String = journal
            .conn
            .query_row("PRAGMA journal_mode;", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "wal");

        for table in [
            "trade_events",
            "order_states",
            "circuit_breaker_events",
            "regime_transitions",
            "system_events",
        ] {
            let count: i64 = journal
                .conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    params![table],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "table {table} should exist");
        }
    }

    /// 6.2 — Each variant routes to the correct table.
    #[test]
    fn event_routing_per_variant() {
        let (mut journal, _dir) = make_journal();

        journal.write_event(&sample_trade(Some(11))).unwrap();
        journal.write_event(&sample_order_state()).unwrap();
        journal.write_event(&sample_breaker()).unwrap();
        journal.write_event(&sample_regime()).unwrap();
        journal.write_event(&sample_system()).unwrap();

        let count_of = |table: &str| -> i64 {
            journal
                .conn
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                    row.get(0)
                })
                .unwrap()
        };

        assert_eq!(count_of("trade_events"), 1);
        assert_eq!(count_of("order_states"), 1);
        assert_eq!(count_of("circuit_breaker_events"), 1);
        assert_eq!(count_of("regime_transitions"), 1);
        assert_eq!(count_of("system_events"), 1);
    }

    /// 6.3 — Channel backpressure: full channel drops events, no blocking.
    #[test]
    fn channel_backpressure_drops_when_full() {
        // Use a tiny custom channel to provoke the full state without enqueueing 8192 events.
        let (tx, rx) = crossbeam_channel::bounded::<EngineEvent>(2);
        let sender = JournalSender {
            inner: tx,
            source_override: None,
        };

        assert!(sender.send(sample_system()));
        assert!(sender.send(sample_system()));
        // Channel is full — third send must drop and return false (not block).
        let start = Instant::now();
        let accepted = sender.send(sample_system());
        let elapsed = start.elapsed();
        assert!(!accepted, "third send should be dropped");
        assert!(
            elapsed < Duration::from_millis(50),
            "send must not block (took {elapsed:?})"
        );

        // Drain to keep rx alive until end of test.
        let _ = rx.try_recv();
    }

    /// 6.4 — Timestamps stored as Unix nanos and prices stored as quarter-tick integers.
    #[test]
    fn integer_storage_for_timestamps_and_prices() {
        let (mut journal, _dir) = make_journal();
        let ts = UnixNanos::new(1_700_000_000_123_456_789);
        let px = FixedPrice::new(17929); // 4482.25

        journal
            .write_event(&EngineEvent::TradeEvent(TradeEventRecord {
                timestamp: ts,
                decision_id: Some(1),
                order_id: Some(1),
                symbol_id: 1,
                side: Side::Sell,
                price: px,
                size: 3,
                kind: "fill".to_string(),
                source: TradeSource::Live,
            }))
            .unwrap();

        let (stored_ts, stored_px): (i64, i64) = journal
            .conn
            .query_row(
                "SELECT timestamp, price FROM trade_events LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored_ts as u64, ts.as_nanos());
        assert_eq!(stored_px, px.raw());
    }

    /// 6.5 — Trade events without decision_id are still persisted but logged as an error.
    /// (The journal MUST NOT silently drop trade events; missing decision_id surfaces as
    /// an error log per Task 4.5.)
    #[test]
    fn trade_event_without_decision_id_is_logged_but_persisted() {
        let (mut journal, _dir) = make_journal();
        journal.write_event(&sample_trade(None)).unwrap();
        let count: i64 = journal
            .conn
            .query_row("SELECT count(*) FROM trade_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
    }

    /// 6.5b — Trade events with decision_id round-trip correctly.
    #[test]
    fn trade_event_decision_id_round_trip() {
        let (mut journal, _dir) = make_journal();
        journal.write_event(&sample_trade(Some(99))).unwrap();
        let id: i64 = journal
            .conn
            .query_row("SELECT decision_id FROM trade_events LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(id, 99);
    }

    /// 6.6 — WAL checkpoint runs without error on a populated database.
    #[test]
    fn wal_checkpoint_succeeds_on_populated_db() {
        let (mut journal, _dir) = make_journal();
        for i in 0..50 {
            journal
                .write_event(&EngineEvent::SystemEvent(SystemEventRecord {
                    timestamp: UnixNanos::new(i),
                    category: "test".into(),
                    message: format!("evt {i}"),
                }))
                .unwrap();
        }
        let (_log, _ckpt) = journal.checkpoint().expect("checkpoint should succeed");
    }

    /// 6.7 — Crash recovery: drop connection without checkpoint, reopen, data intact.
    #[test]
    fn crash_recovery_preserves_committed_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.db");

        {
            let mut journal = EventJournal::new(&path).unwrap();
            for i in 0..10 {
                journal
                    .write_event(&EngineEvent::SystemEvent(SystemEventRecord {
                        timestamp: UnixNanos::new(1_000 + i),
                        category: "test".into(),
                        message: format!("evt {i}"),
                    }))
                    .unwrap();
            }
            // Drop without explicit checkpoint to simulate process crash.
        }

        let journal = EventJournal::new(&path).unwrap();
        let count: i64 = journal
            .conn
            .query_row("SELECT count(*) FROM system_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 10, "committed events must survive simulated crash");
    }

    /// End-to-end channel + worker loop: producer enqueues, worker drains and persists.
    #[test]
    fn channel_run_loop_persists_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.db");
        let mut journal = EventJournal::new(&path).unwrap();
        let (sender, receiver) = EventJournal::channel();

        // Spawn a producer thread that emits a handful of events then drops the sender.
        let producer = thread::spawn(move || {
            for i in 0..20 {
                sender.send(EngineEvent::SystemEvent(SystemEventRecord {
                    timestamp: UnixNanos::new(2_000 + i),
                    category: "loop".into(),
                    message: format!("e{i}"),
                }));
            }
            // sender is moved into closure; drop here ends the run loop.
        });

        // Run the worker on this thread (it returns when senders disconnect).
        journal.run(receiver).expect("worker run");
        producer.join().unwrap();

        let count: i64 = journal
            .conn
            .query_row("SELECT count(*) FROM system_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 20);
    }

    /// JournalSender is Send + Clone so multiple producers can emit events.
    #[test]
    fn journal_sender_is_send_and_clone() {
        fn assert_send_clone<T: Send + Clone>() {}
        assert_send_clone::<JournalSender>();
    }

    /// Regression for review finding B-1: the run loop must NOT exit on a transient
    /// empty queue, and must NOT consume-and-discard events. Producer sends events
    /// with deliberate gaps spanning multiple BATCH_INTERVAL windows; the journal
    /// must persist every single one.
    #[test]
    fn run_loop_survives_transient_empty_and_persists_all_events() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.db");
        let mut journal = EventJournal::new(&path).unwrap();
        let (sender, receiver) = EventJournal::channel();

        const N: u64 = 8;
        // Sleep substantially longer than BATCH_INTERVAL (100 ms) between sends to
        // guarantee the worker observes RecvTimeoutError::Timeout — and previously
        // also is_empty() == true with try_recv() == Err(Empty), which the buggy
        // is_disconnected() helper conflated with the terminal signal.
        let gap = BATCH_INTERVAL * 3; // 300 ms

        let producer = thread::spawn(move || {
            for i in 0..N {
                sender.send(EngineEvent::SystemEvent(SystemEventRecord {
                    timestamp: UnixNanos::new(3_000 + i),
                    category: "gap".into(),
                    message: format!("g{i}"),
                }));
                thread::sleep(gap);
            }
            // Sender dropped here — recv_timeout will return Disconnected once drained.
        });

        journal.run(receiver).expect("worker run");
        producer.join().unwrap();

        let count: i64 = journal
            .conn
            .query_row(
                "SELECT count(*) FROM system_events WHERE category = 'gap'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, N as i64,
            "every event must be persisted even when the channel briefly empties"
        );
    }

    /// Story 7.4 Task 1.1/1.2 — fresh journals create the trade_events and
    /// order_states tables with a `source` column that defaults to 'live'.
    #[test]
    fn fresh_journal_has_source_columns_defaulting_to_live() {
        let (journal, _dir) = make_journal();
        for table in ["trade_events", "order_states"] {
            // PRAGMA table_info — column `name` is index 1, `dflt_value` is 4.
            let mut stmt = journal
                .conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .unwrap();
            let mut found_source = false;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(1)?, row.get::<_, Option<String>>(4)?))
                })
                .unwrap();
            for row in rows {
                let (name, default) = row.unwrap();
                if name == "source" {
                    found_source = true;
                    let dflt = default.unwrap_or_default();
                    assert!(
                        dflt.contains("live"),
                        "{table}.source default must be 'live', got {dflt:?}"
                    );
                }
            }
            assert!(
                found_source,
                "{table} must have a `source` column post-Story 7.4"
            );
        }
    }

    /// Story 7.4 Task 1.5 — source-column index exists for efficient
    /// WHERE source = ? filtering.
    #[test]
    fn source_column_indexes_exist() {
        let (journal, _dir) = make_journal();
        for index in ["idx_trade_events_source", "idx_order_states_source"] {
            let count: i64 = journal
                .conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='index' AND name=?1",
                    params![index],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "expected source index {index} to exist");
        }
    }

    /// Story 7.4 Task 1 — migration backfills the source column on a journal
    /// created with the pre-7.4 schema (no source column). Simulates the
    /// upgrade path: old DB on disk gets the column added on next open and
    /// existing rows default to 'live'.
    #[test]
    fn migration_adds_source_column_to_pre_story_7_4_journal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("journal.db");

        // Create a journal with the pre-7.4 schema (no source column) and
        // insert one row so the migration has data to coerce.
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE trade_events (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    timestamp INTEGER NOT NULL,
                    decision_id INTEGER NOT NULL,
                    order_id INTEGER,
                    symbol_id INTEGER NOT NULL,
                    side INTEGER NOT NULL,
                    price INTEGER NOT NULL,
                    size INTEGER NOT NULL,
                    kind TEXT NOT NULL
                );
                CREATE TABLE order_states (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    timestamp INTEGER NOT NULL,
                    order_id INTEGER NOT NULL,
                    decision_id INTEGER,
                    from_state TEXT NOT NULL,
                    to_state TEXT NOT NULL
                );",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO trade_events
                    (timestamp, decision_id, order_id, symbol_id, side, price, size, kind)
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                params![1i64, 1i64, 1i64, 1i64, 0i64, 100i64, 1i64, "fill"],
            )
            .unwrap();
        }

        // Re-open through the production path — migration runs, source column
        // is added, the existing row backfills to 'live'.
        let _journal = EventJournal::new(&path).unwrap();
        let conn = Connection::open(&path).unwrap();
        let source: String = conn
            .query_row("SELECT source FROM trade_events LIMIT 1", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(source, "live", "pre-7.4 row backfilled to 'live' source");
    }

    /// Story 7.4 Task 2 — sender override stamps the source onto trade /
    /// order records before they enter the channel. Other variant types
    /// (system events, breakers, regime transitions) are not source-tagged
    /// and pass through unchanged.
    #[test]
    fn sender_override_stamps_trade_and_order_records_only() {
        let (paper_sender, paper_rx) = EventJournal::channel_with_source(TradeSource::Paper);

        let trade = EngineEvent::TradeEvent(TradeEventRecord {
            timestamp: UnixNanos::new(1),
            decision_id: Some(1),
            order_id: Some(1),
            symbol_id: 1,
            side: Side::Buy,
            price: FixedPrice::new(100),
            size: 1,
            kind: "fill".into(),
            source: TradeSource::Live, // sender will rewrite
        });
        let order = EngineEvent::OrderStateChange(OrderStateChangeRecord {
            timestamp: UnixNanos::new(2),
            order_id: 1,
            decision_id: Some(1),
            from_state: "Idle".into(),
            to_state: "Submitted".into(),
            source: TradeSource::Live, // sender will rewrite
        });
        let system = EngineEvent::SystemEvent(SystemEventRecord {
            timestamp: UnixNanos::new(3),
            category: "test".into(),
            message: "msg".into(),
        });
        assert!(paper_sender.send(trade));
        assert!(paper_sender.send(order));
        assert!(paper_sender.send(system));

        // Trade and order get rewritten; system event passes through.
        let mut saw_trade_paper = false;
        let mut saw_order_paper = false;
        let mut saw_system = false;
        for _ in 0..8 {
            match paper_rx.try_recv_for_test() {
                Some(EngineEvent::TradeEvent(r)) => {
                    assert_eq!(r.source, TradeSource::Paper);
                    saw_trade_paper = true;
                }
                Some(EngineEvent::OrderStateChange(r)) => {
                    assert_eq!(r.source, TradeSource::Paper);
                    saw_order_paper = true;
                }
                Some(EngineEvent::SystemEvent(_)) => saw_system = true,
                Some(_) => {}
                None => break,
            }
        }
        assert!(saw_trade_paper);
        assert!(saw_order_paper);
        assert!(saw_system);
    }
}
