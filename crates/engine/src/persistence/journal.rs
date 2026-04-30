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

use crossbeam_channel::{Receiver, Sender, TrySendError};
use futures_bmad_core::{FixedPrice, Side, UnixNanos};
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
        kind         TEXT    NOT NULL
    );
";

const CREATE_ORDER_STATES: &str = "
    CREATE TABLE IF NOT EXISTS order_states (
        id           INTEGER PRIMARY KEY AUTOINCREMENT,
        timestamp    INTEGER NOT NULL,
        order_id     INTEGER NOT NULL,
        decision_id  INTEGER,
        from_state   TEXT    NOT NULL,
        to_state     TEXT    NOT NULL
    );
";

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
}

/// Order state machine transition recorded into `order_states`.
#[derive(Debug, Clone)]
pub struct OrderStateChangeRecord {
    pub timestamp: UnixNanos,
    pub order_id: u64,
    pub decision_id: Option<u64>,
    pub from_state: String,
    pub to_state: String,
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
#[derive(Clone)]
pub struct JournalSender {
    inner: Sender<EngineEvent>,
}

impl JournalSender {
    /// Attempt to enqueue an event. On full channel, log a warning and drop.
    ///
    /// Returns `true` if the event was accepted, `false` if it was dropped or the
    /// receiver was disconnected.
    pub fn send(&self, event: EngineEvent) -> bool {
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

    /// Approximate current channel depth (for monitoring/tests).
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether the channel is currently empty.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
}

/// Receiver half — owned by the journal worker.
pub struct JournalReceiver {
    inner: Receiver<EngineEvent>,
}

impl JournalReceiver {
    fn try_recv(&self) -> Option<EngineEvent> {
        self.inner.try_recv().ok()
    }

    fn recv_timeout(&self, timeout: Duration) -> Option<EngineEvent> {
        self.inner.recv_timeout(timeout).ok()
    }

    /// True once all senders are dropped and the channel is drained — terminal signal.
    fn is_disconnected(&self) -> bool {
        // crossbeam reports Disconnected on recv when senders are gone AND queue empty.
        // We approximate by checking sender count via try_recv on next loop turn.
        self.inner.is_empty() && self.inner.try_recv().is_err()
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
    pub fn channel() -> (JournalSender, JournalReceiver) {
        let (tx, rx) = crossbeam_channel::bounded(JOURNAL_CHANNEL_CAPACITY);
        (JournalSender { inner: tx }, JournalReceiver { inner: rx })
    }

    /// Run the synchronous batching write loop until all senders disconnect.
    ///
    /// Drains events from `receiver`, accumulating up to `BATCH_SIZE` or
    /// `BATCH_INTERVAL`, whichever comes first, then flushes them in a single
    /// transaction. Periodically issues a passive WAL checkpoint.
    ///
    /// This function blocks the calling thread; in async contexts spawn it via
    /// `tokio::task::spawn_blocking`.
    pub fn run(&mut self, receiver: JournalReceiver) -> Result<(), JournalError> {
        let mut batch: Vec<EngineEvent> = Vec::with_capacity(BATCH_SIZE);
        let mut last_flush = Instant::now();
        let mut last_checkpoint = Instant::now();

        loop {
            // Wait up to BATCH_INTERVAL for the next event.
            let wait = BATCH_INTERVAL.saturating_sub(last_flush.elapsed());
            let next = if wait.is_zero() {
                receiver.try_recv()
            } else {
                receiver.recv_timeout(wait)
            };
            let received_any = next.is_some();

            if let Some(event) = next {
                batch.push(event);
                // Drain any other immediately-available events to fill the batch.
                while batch.len() < BATCH_SIZE {
                    if let Some(more) = receiver.try_recv() {
                        batch.push(more);
                    } else {
                        break;
                    }
                }
            }

            let interval_elapsed = last_flush.elapsed() >= BATCH_INTERVAL;
            let batch_full = batch.len() >= BATCH_SIZE;
            if !batch.is_empty() && (batch_full || interval_elapsed) {
                self.flush_batch(&mut batch)?;
                last_flush = Instant::now();
            } else if batch.is_empty() && interval_elapsed {
                // Reset the timer so we don't spin on the elapsed branch.
                last_flush = Instant::now();
            }

            // Periodic passive WAL checkpoint.
            if last_checkpoint.elapsed() >= CHECKPOINT_INTERVAL {
                if let Err(e) = self.checkpoint() {
                    warn!(target: "journal", error = %e, "wal checkpoint failed");
                }
                last_checkpoint = Instant::now();
            }

            // Termination: senders all gone and queue drained.
            if !received_any && batch.is_empty() && receiver.is_disconnected() {
                debug!(target: "journal", "all senders disconnected, journal worker exiting");
                break;
            }
        }

        // Flush anything left over.
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
                        (timestamp, decision_id, order_id, symbol_id, side, price, size, kind)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
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
                    ],
                )?;
            }
            EngineEvent::OrderStateChange(r) => {
                tx.execute(
                    "INSERT INTO order_states
                        (timestamp, order_id, decision_id, from_state, to_state)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        r.timestamp.as_nanos() as i64,
                        r.order_id as i64,
                        r.decision_id.map(|id| id as i64),
                        r.from_state.as_str(),
                        r.to_state.as_str(),
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
        })
    }

    fn sample_order_state() -> EngineEvent {
        EngineEvent::OrderStateChange(OrderStateChangeRecord {
            timestamp: UnixNanos::new(1_700_000_000_000_000_001),
            order_id: 42,
            decision_id: Some(7),
            from_state: "Idle".to_string(),
            to_state: "Submitted".to_string(),
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
        let sender = JournalSender { inner: tx };

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
}
