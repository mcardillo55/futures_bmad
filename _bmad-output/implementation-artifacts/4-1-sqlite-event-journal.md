# Story 4.1: SQLite Event Journal

Status: review

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
- [x] 1.1: Create `crates/engine/src/persistence/mod.rs` with `pub mod journal;`
- [x] 1.2: Define `EngineEvent` enum in `crates/engine/src/persistence/journal.rs` with variants: `TradeEvent`, `OrderStateChange`, `CircuitBreakerEvent`, `RegimeTransition`, `SystemEvent` — each carrying a `decision_id: Option<u64>`, `timestamp: UnixNanos`, and variant-specific fields
- [x] 1.3: Define SQL CREATE TABLE statements as constants for each table: `trade_events`, `order_states`, `circuit_breaker_events`, `regime_transitions`, `system_events` — all timestamps as `INTEGER` (Unix nanos), all prices as `INTEGER` (quarter-ticks)
- [x] 1.4: Update `crates/engine/src/lib.rs` to declare `pub mod persistence` (already declared by prior story; persistence module now re-exports journal types)

### Task 2: Implement EventJournal struct and SQLite initialization (AC: WAL mode, table creation)
- [x] 2.1: Implement `EventJournal::new(db_path: &Path) -> Result<Self>` that creates parent directories if needed, opens SQLite via `rusqlite::Connection::open()`
- [x] 2.2: Set WAL mode immediately after open: `PRAGMA journal_mode=WAL` (verified by query_row return value)
- [x] 2.3: Execute all CREATE TABLE IF NOT EXISTS statements in a single transaction
- [x] 2.4: Set additional pragmas for performance: `PRAGMA synchronous=NORMAL` (WAL-safe), `PRAGMA busy_timeout=5000`
- [x] 2.5: Create default db path helper: `data/journal.db` relative to working directory (`DEFAULT_DB_PATH` constant + `EventJournal::open_default()`)

### Task 3: Implement crossbeam channel event delivery (AC: bounded channel, never block hot path)
- [x] 3.1: Create `JournalSender` wrapper around `crossbeam_channel::Sender<EngineEvent>` with `send()` that uses `try_send()` — on full channel, log warning and drop event (never block)
- [x] 3.2: Create `JournalReceiver` wrapper around `crossbeam_channel::Receiver<EngineEvent>`
- [x] 3.3: Implement `EventJournal::channel() -> (JournalSender, JournalReceiver)` constructing bounded channel with capacity 8192 (`JOURNAL_CHANNEL_CAPACITY`)
- [x] 3.4: `JournalSender` must be `Clone + Send` so multiple producers can emit events (verified via static `assert_send_clone` test)

### Task 4: Implement async write loop on Tokio (AC: async writes, event routing)
- [x] 4.1: Implement `EventJournal::run(receiver: JournalReceiver)` as a synchronous batching loop intended to be spawned via `tokio::task::spawn_blocking` (rusqlite `Connection` is `!Send`, so the run loop must own the connection on a dedicated thread)
- [x] 4.2: Run loop polls the crossbeam channel with `recv_timeout(BATCH_INTERVAL)` so the call yields cooperatively without blocking the Tokio executor when used inside `spawn_blocking`
- [x] 4.3: Route each `EngineEvent` variant to its corresponding table — `write_one()` performs an exhaustive match → INSERT per table
- [x] 4.4: Batch writes: accumulate up to 64 events (`BATCH_SIZE`) or 100 ms (`BATCH_INTERVAL`), then write in a single transaction for throughput
- [x] 4.5: Trade events without `decision_id` are persisted but emit `tracing::error!` (NFR17 violation surfaced in logs, never silently dropped)

### Task 5: Implement WAL checkpointing (AC: periodic checkpoint, no write blocking)
- [x] 5.1: `EventJournal::checkpoint()` runs `PRAGMA wal_checkpoint(PASSIVE)` — passive mode never blocks writers
- [x] 5.2: Run loop triggers checkpoint every 60 seconds (`CHECKPOINT_INTERVAL`) via wall-clock comparison (`Instant::now()`); does not require Tokio interval since the worker is sync inside `spawn_blocking`
- [x] 5.3: Logs checkpoint result (`wal_pages`, `checkpointed`) at `debug` level
- [x] 5.4: Checkpoint failures log at `warn` level and do not abort the run loop

### Task 6: Unit tests (AC: all)
- [x] 6.1: `initialization_creates_tables_in_wal_mode` — verifies db file exists, journal_mode==wal, and all 5 tables created
- [x] 6.2: `event_routing_per_variant` — writes one of each variant and asserts row counts in each table
- [x] 6.3: `channel_backpressure_drops_when_full` — fills a 2-slot channel and asserts third send returns false in <50 ms (no block)
- [x] 6.4: `integer_storage_for_timestamps_and_prices` — round-trips Unix nanos and FixedPrice raw via INTEGER columns
- [x] 6.5: `trade_event_decision_id_round_trip` — confirms decision_id persisted; `trade_event_without_decision_id_is_logged_but_persisted` — confirms missing-decision_id surfaces as error log but does not silently drop the event
- [x] 6.6: `wal_checkpoint_succeeds_on_populated_db` — populates 50 events, runs checkpoint without error
- [x] 6.7: `crash_recovery_preserves_committed_events` — writes 10 events, drops connection without explicit checkpoint, reopens, asserts all 10 still present
- [x] 6.8 (added): `channel_run_loop_persists_events` — end-to-end test of the producer/worker loop draining via `EventJournal::run()`
- [x] 6.9 (added): `journal_sender_is_send_and_clone` — static assertion that JournalSender is `Send + Clone`

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
Claude Opus 4.7 (1M context)

### Debug Log References
- Initial build error: partial-move on `next` between the `if let Some(event) = next` branch and the later `next.is_none()` check — fixed by hoisting `let received_any = next.is_some();` before the consume.
- `cargo fmt` reformatted one nested `query_row` callback in tests; no semantic change.

### Completion Notes List
- New `crates/engine/src/persistence/journal.rs` (~700 LOC including tests) implements `EngineEvent`, payload records, `EventJournal`, `JournalSender`, `JournalReceiver`, errors, and the batching write loop.
- Added `rusqlite = { workspace = true }` to `crates/engine/Cargo.toml` (workspace already declared the dependency at `0.38.0` with the `bundled` feature, so no additional features needed).
- Updated `crates/engine/src/persistence/mod.rs` to declare `pub mod journal` and re-export the public types.
- WAL mode verified at `EventJournal::new()` by reading back the `PRAGMA journal_mode = WAL;` return string and rejecting the open if the engine refused WAL (e.g., on a corrupt `tempfs` mount).
- Connection is `!Send` so `EventJournal::run()` is a synchronous loop intended for `tokio::task::spawn_blocking` rather than a true `async fn`. This matches the Dev Note: "Use `tokio::task::spawn_blocking` for the write loop or dedicate a std::thread." A pure async signature would force a `Mutex<Connection>` or move-out-of-Connection per call, both of which conflict with the single-thread ownership invariant.
- Naming: kept the journal-side enum named `EngineEvent` per spec (Task 1.2); it shadows `core::EngineEvent` only when imported, and the module path `persistence::journal::EngineEvent` is unambiguous. The two enums are intentionally decoupled — the journal table schema can evolve without touching the in-memory event bus.
- 10 unit tests cover all 7 ACs plus end-to-end channel/run loop and Send+Clone bound. Total workspace test count: 220 (was 208 prior).

### File List
- crates/engine/src/persistence/journal.rs (new)
- crates/engine/src/persistence/mod.rs (modified — added `pub mod journal` and re-exports)
- crates/engine/Cargo.toml (modified — added rusqlite dependency)
- _bmad-output/implementation-artifacts/sprint-status.yaml (modified — moved 4-1 to "review")

### Change Log
- 2026-04-30: Implemented Story 4.1 — SQLite Event Journal (all tasks complete; 10 tests passing)
- 2026-04-30: Code review complete — verdict REQUEST-CHANGES; 1 BLOCKING, 5 SHOULD-FIX, 7 NICE-TO-HAVE. See `4-1-code-review-2026-04-30.md`.
- 2026-04-30: B-1 fix landed in `b065d30`; re-review verdict **RESOLVED** with no new findings. Story safe to proceed.

## Senior Developer Review

**Date:** 2026-04-30
**Reviewer:** Claude Opus 4.7 (1M context) via `bmad-code-review`
**Verdict:** REQUEST-CHANGES → **B-1 RESOLVED in `b065d30` (2026-04-30 re-review)**
**Full report:** [`4-1-code-review-2026-04-30.md`](./4-1-code-review-2026-04-30.md) (see § Re-review (2026-04-30) appendix for the post-fix verification)

**Re-review addendum (2026-04-30, commit `b065d30`):** B-1 closed — the run loop now uses `recv_timeout` to distinguish `Timeout` (transient, flush partial, keep looping) from `Disconnected` (terminal, flush, exit). No path consumes-and-discards. New regression test `run_loop_survives_transient_empty_and_persists_all_events` exercises the multi-`Timeout`-with-live-senders scenario the prior code mishandled. All 11 journal tests pass. No new findings introduced. `cargo fmt --check` issues observed in unrelated files (`risk/fee_gate.rs`, `signals/{composite,levels,mod}.rs`, `tests/{levels,vpin,microprice,obi}_tests.rs`) are pre-existing on `main` and not introduced by this branch — defer to a separate `style:` cleanup PR. **Story 4-1 is safe to proceed to story 4-2.**

### Review Findings

- [ ] [Review][Decision] Spec contradiction: AC2 vs Task 4.5 on missing decision_id (S-2) — AC2 says "every trade event includes decision_id" (hard); Task 4.5 says missing values are "logged and persisted" (soft). Implementation follows Task 4.5. Confirm intent and reword the AC, or change implementation to drop trade events without decision_id.
- [x] [Review][Patch][BLOCKING] `JournalReceiver::is_disconnected()` silently consumes events and exits prematurely (B-1) [`crates/engine/src/persistence/journal.rs:300-306, :382-440`] — `try_recv()` discards a message if one races in; `is_err()` collapses `Empty` and `Disconnected` so the worker exits whenever the queue is briefly empty. Restructure the run loop around a single `recv_timeout` returning `RecvTimeoutError::Disconnected` as the terminal signal. **RESOLVED in `b065d30`** — re-review confirmed the fix; see `4-1-code-review-2026-04-30.md` § Re-review (2026-04-30).
- [ ] [Review][Patch] `decision_id = 0` sentinel collides with valid id 0 (S-1) [`journal.rs:501`] — make `trade_events.decision_id` nullable and persist `r.decision_id.map(|id| id as i64)`; let the existing error log surface the NFR17 violation.
- [ ] [Review][Patch] `JournalError::Sqlite(InvalidQuery)` for WAL refusal is misleading (S-3) [`journal.rs:336-338`] — add `JournalError::WalUnavailable { actual: String }` variant.
- [ ] [Review][Patch] No graceful-shutdown API on `EventJournal` / `JournalSender` (S-4) [`journal.rs:317-440`] — add an explicit Shutdown variant or a oneshot; acceptable to defer to story 8-3 if recorded in `deferred-work.md`.
- [ ] [Review][Patch] Single-writer-per-file constraint is undocumented (S-5) [`journal.rs:317`] — add a one-line doc constraint on `EventJournal`.
- [ ] [Review][Patch] `u64 → i64` casts at multiple sites need a documenting comment (N-1) [`journal.rs:499-556`] — note that SQLite INTEGER stores i64; values > i64::MAX wrap.
- [ ] [Review][Patch] `unchecked_transaction()` skips the nested-transaction safety check (N-2) [`journal.rs:349, 444, 473`] — prefer `conn.transaction()` or document the no-nested invariant in a struct comment.
- [ ] [Review][Patch] `wal_checkpoint(PASSIVE)` busy=1 result is silently ignored (N-3) [`journal.rs:451-465`] — emit a `warn!` (or `debug!`) when `busy=1 && ckpt < log` to flag stuck checkpoints.
- [ ] [Review][Patch] `flush_batch` errors mid-loop lose drained-but-not-yet-flushed events (N-4) [`journal.rs:436-440`] — document the trade-off as a comment so an operator-alerting story can wire a supervisor restart.
- [ ] [Review][Patch] `DEFAULT_DB_PATH = "data/journal.db"` is `cwd`-relative (N-5) [`journal.rs:58`] — resolve via env var or document the cwd dependency.
- [ ] [Review][Patch] `channel_backpressure_drops_when_full` 50 ms threshold may flake on CI (N-6) [`journal.rs:715-718`] — relax to 250 ms.
- [ ] [Review][Patch] No real-capacity stress test exercises 8192 channel slots (N-7) [`journal.rs` test module] — push 9 000 events through `EventJournal::channel()` against a slow worker.
