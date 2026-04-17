# Story 4.1: SQLite Event Journal

Status: ready-for-dev

## Story

As a trader-operator,
I want all system events persisted to a durable journal,
So that I have a complete audit trail and can recover state after crashes.

## Acceptance Criteria (BDD)

- Given `engine/src/persistence/journal.rs` When initialized Then creates/opens SQLite at `data/journal.db` in WAL mode, tables created if not exist: trade_events, order_states, circuit_breaker_events, regime_transitions, system_events. Timestamps as INTEGER (Unix nanos), prices as INTEGER (quarter-ticks)
- Given journal receives events via bounded crossbeam channel (capacity 8192) When EngineEvent variants written Then each routed to appropriate table, writes async on Tokio (never block hot path), every trade event includes decision_id
- Given system crashes When restarts Then SQLite WAL ensures no committed data lost, uncommitted in-flight events (last few ms) may be lost — acceptable
- Given journal running When periodic WAL checkpointing occurs Then WAL file checkpointed to prevent unbounded growth, checkpointing doesn't block writes

## Tasks / Subtasks

### Task 1: Define EngineEvent enum and journal table schemas (AC: tables, timestamps, prices)
- 1.1: Create `crates/engine/src/persistence/mod.rs` with `pub mod journal;`
- 1.2: Define `EngineEvent` enum in `crates/engine/src/persistence/journal.rs` with variants: `TradeEvent`, `OrderStateChange`, `CircuitBreakerEvent`, `RegimeTransition`, `SystemEvent` — each carrying a `decision_id: Option<u64>`, `timestamp: UnixNanos`, and variant-specific fields
- 1.3: Define SQL CREATE TABLE statements as constants for each table: `trade_events`, `order_states`, `circuit_breaker_events`, `regime_transitions`, `system_events` — all timestamps as `INTEGER` (Unix nanos), all prices as `INTEGER` (quarter-ticks)
- 1.4: Update `crates/engine/src/lib.rs` to declare `pub mod persistence`

### Task 2: Implement EventJournal struct and SQLite initialization (AC: WAL mode, table creation)
- 2.1: Implement `EventJournal::new(db_path: &Path) -> Result<Self>` that creates parent directories if needed, opens SQLite via `rusqlite::Connection::open()`
- 2.2: Set WAL mode immediately after open: `PRAGMA journal_mode=WAL`
- 2.3: Execute all CREATE TABLE IF NOT EXISTS statements in a single transaction
- 2.4: Set additional pragmas for performance: `PRAGMA synchronous=NORMAL` (WAL-safe), `PRAGMA busy_timeout=5000`
- 2.5: Create default db path helper: `data/journal.db` relative to working directory

### Task 3: Implement crossbeam channel event delivery (AC: bounded channel, never block hot path)
- 3.1: Create `JournalSender` wrapper around `crossbeam_channel::Sender<EngineEvent>` with `send()` that uses `try_send()` — on full channel, log warning and drop event (never block)
- 3.2: Create `JournalReceiver` wrapper around `crossbeam_channel::Receiver<EngineEvent>`
- 3.3: Implement `EventJournal::channel() -> (JournalSender, JournalReceiver)` constructing bounded channel with capacity 8192
- 3.4: `JournalSender` must be `Clone + Send` so multiple producers can emit events

### Task 4: Implement async write loop on Tokio (AC: async writes, event routing)
- 4.1: Implement `EventJournal::run(receiver: JournalReceiver)` as an async method spawned on the Tokio runtime
- 4.2: In the run loop, receive events from the crossbeam channel (use `recv()` in a `tokio::task::spawn_blocking` or poll with timeout to avoid blocking the Tokio executor)
- 4.3: Route each `EngineEvent` variant to its corresponding table via prepared statements (one per table, prepared at startup for efficiency)
- 4.4: Batch writes: accumulate up to 64 events or 100ms, then write in a single transaction for throughput
- 4.5: Ensure every trade-related event includes `decision_id` — log error if `decision_id` is None for TradeEvent variants

### Task 5: Implement WAL checkpointing (AC: periodic checkpoint, no write blocking)
- 5.1: Implement periodic WAL checkpoint using `PRAGMA wal_checkpoint(PASSIVE)` — passive mode never blocks writers
- 5.2: Schedule checkpoint every 60 seconds via a Tokio interval timer
- 5.3: Log checkpoint result (pages checkpointed, WAL size) at `debug` level
- 5.4: If checkpoint fails, log at `warn` level but do not halt — WAL will grow but system remains operational

### Task 6: Unit tests (AC: all)
- 6.1: Test journal initialization creates database file and all tables in WAL mode
- 6.2: Test event routing — each EngineEvent variant written to correct table
- 6.3: Test channel backpressure — full channel drops events without blocking, warning logged
- 6.4: Test timestamps stored as Unix nanos and prices as quarter-tick integers
- 6.5: Test decision_id is present on all trade events
- 6.6: Test WAL checkpoint runs without error on a populated database
- 6.7: Test crash recovery — write events, simulate crash (drop connection without checkpoint), reopen and verify committed data intact

## Dev Notes

### Architecture Patterns & Constraints
- The journal lives in the engine crate but receives events from all crates via the crossbeam channel — the hot path (event loop thread) does a non-blocking `try_send()` and never waits
- rusqlite `Connection` is not `Send` — the journal's SQLite connection must live on a single dedicated thread/task. Use `tokio::task::spawn_blocking` for the write loop or dedicate a std::thread
- WAL mode with `synchronous=NORMAL` provides crash safety: committed transactions survive process crash, only OS crash could lose data (acceptable per NFR9)
- Batch writes in transactions reduce fsync overhead — target 64 events or 100ms batching window
- All table names are snake_case, plural (e.g., `trade_events`, not `TradeEvent`)
- `decision_id` is the causality tracing key (NFR17) — every order, fill, and bracket event chains back to the signal that originated it

### Project Structure Notes
```
crates/engine/
├── src/
│   ├── lib.rs
│   └── persistence/
│       ├── mod.rs
│       └── journal.rs
```

### References
- Architecture document: `_bmad-output/planning-artifacts/architecture.md` — Persistence, Event Journal
- Epics document: `_bmad-output/planning-artifacts/epics.md` — Epic 4, Story 4.1
- Dependencies: rusqlite 0.38.0, crossbeam-channel 0.5.15, tokio 1.51.1, tracing 0.1.44
- NFR9: Crash safety via WAL
- NFR17: Causality tracing via decision_id

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
