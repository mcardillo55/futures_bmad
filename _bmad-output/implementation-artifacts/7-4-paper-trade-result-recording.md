# Story 7.4: Paper Trade Result Recording

Status: done

## Story

As a trader-operator,
I want paper trade results recorded with the same fidelity as live trades,
So that I can analyze paper trading performance with the same tools as live.

## Acceptance Criteria (BDD)

- Given paper trading active When simulated fills occur Then written to SQLite journal with `source: paper` tag, all fields match live recording (order_id, fill_price, fill_size, timestamp, side, decision_id, P&L)
- Given paper trade results in journal When queried Then P&L, win rate, max drawdown, trade count computable, paper distinguishable from live by source tag
- Given validation progression (unit->replay->paper->live) When paper shows positive expectancy over 500+ trades Then journal contains sufficient data for go/no-go decision, all evidence traceable via decision_id

## Tasks / Subtasks

### Task 1: Add source column to journal tables (AC: source tag distinguishes paper from live)
- [x] 1.1: Add `source TEXT NOT NULL DEFAULT 'live'` column to `trade_events` table in `crates/engine/src/persistence/journal.rs` — values: `'paper'`, `'live'`, `'replay'`
- [x] 1.2: Add `source TEXT NOT NULL DEFAULT 'live'` column to `order_states` table
- [x] 1.3: Update all CREATE TABLE statements to include the source column
- [x] 1.4: Update all INSERT statements to include source value
- [x] 1.5: Add index on source column for efficient filtering: `CREATE INDEX IF NOT EXISTS idx_trade_events_source ON trade_events(source)` (also added matching index on `order_states`)

### Task 2: Propagate source tag through event pipeline (AC: paper fills tagged correctly)
- [x] 2.1: Add `source: TradeSource` field to `EngineEvent` enum variants that record trade/order data — defined `enum TradeSource { Live, Paper, Replay }` in `crates/core/src/types/trade_source.rs`
- [x] 2.2: Set `TradeSource` at the BrokerAdapter level: `MockBrokerAdapter::with_source(behavior, TradeSource)` constructor exposes the configured source. The actual journal-record stamping happens via `JournalSender::with_source(source)` — a single seam through which paper / replay orchestrators tag every record without modifying existing call sites in the order manager and bracket manager.
- [x] 2.3: Pass `TradeSource` into `MockBrokerAdapter` at construction time — paper orchestrator passes `Paper`, replay orchestrator passes `Replay`. Both orchestrators also re-tag the supplied `JournalSender` with their respective source on `attach_journal()`.
- [x] 2.4: Journal writer (`write_one`) reads `source` from the record and writes the corresponding `as_str()` value to the SQLite column.

### Task 3: Ensure all fields match live recording fidelity (AC: order_id, fill_price, fill_size, timestamp, side, decision_id, P&L)
- [x] 3.1: Verified `MockBrokerAdapter` and `MockFillSimulator` produce `FillEvent`s with all required fields (`order_id`, `fill_price`, `fill_size`, `timestamp`, `side`, `decision_id`, `fill_type`).
- [x] 3.2: Verified `decision_id` flows unchanged from `OrderEvent` through the SPSC queue, through `MockFillSimulator::synth_fill`, into the resulting `FillEvent`, and into `TradeEventRecord`. New integration test `decision_id_is_unbroken_from_order_to_journal` asserts the full chain.
- [x] 3.3: P&L is computed identically for paper / replay / live by the shared `JournalQuery::pnl_summary` (FIFO leg pairing, FixedPrice raw quarter-ticks, same convention as `replay::TradeStats`). Fees are not yet in scope (no fee model is wired into the journal write today; this carries over to the live-execution wiring story).
- [x] 3.4: New `paper_and_live_trades_differ_only_in_source_tag` integration test writes the SAME template through a paper-tagged sender and a live-tagged sender and asserts every field except `source` matches.

### Task 4: Implement journal query helpers for performance analysis (AC: P&L, win rate, max drawdown, trade count computable)
- [x] 4.1: `JournalQuery::trades_by_source(source) -> Vec<TradeRecord>` — implemented (parameterized SELECT, ORDER BY timestamp).
- [x] 4.2: `JournalQuery::pnl_summary(source) -> PnlSummary` — implemented with `net_pnl`, `win_count`, `loss_count`, `win_rate`, `max_drawdown`, `trade_count`.
- [x] 4.3: `JournalQuery::trade_count(source) -> u32` — implemented (`SELECT COUNT(*)`).
- [x] 4.4: All query functions use `?N` parameterized SQL (no string interpolation of source values).
- [x] 4.5: Created `crates/engine/src/persistence/query.rs`. Re-exported via `persistence::mod` and `paper::mod` for ergonomic access from the orchestrator layer.

### Task 5: Implement decision_id traceability chain (AC: all evidence traceable via decision_id)
- [x] 5.1: Verified — decision_id is generated at signal composite evaluation (Story 3.5) and attached to `OrderEvent` at submission. No changes needed in this story.
- [x] 5.2: Verified end-to-end: `OrderEvent.decision_id → MockFillSimulator → FillEvent.decision_id → TradeEventRecord.decision_id → SQLite trade_events.decision_id`. New integration test asserts the unbroken chain.
- [x] 5.3: `JournalQuery::trace_decision(decision_id) -> DecisionTrace` joins `trade_events` and `order_states` rows sharing the id, regardless of source.
- [x] 5.4: `DecisionTrace` carries chronologically-ordered `Vec<TradeRecord>` and `Vec<OrderStateRecord>` — sufficient for post-hoc forensic analysis.

### Task 6: Implement go/no-go readiness query (AC: sufficient data for deployment decision)
- [x] 6.1: `JournalQuery::paper_readiness_report() -> ReadinessReport` — computes total trades, positive expectancy, win rate, profit factor, max drawdown, Sharpe estimate (mean / sample std-dev, not annualised), consecutive loss max, net P&L.
- [x] 6.2: `ReadinessReport::meets_minimum_threshold(min_trades)` predicate; default 500-trade gate documented in spec but caller-controlled.
- [x] 6.3: `PaperTradingOrchestrator::emit_readiness_summary(&Connection)` logs at `info` level and writes a `paper_readiness_summary` system event to the journal so the audit trail captures the session-end snapshot.
- [x] 6.4: Documented in `ReadinessReport` rustdoc — "informational; the deployment decision is human-made".

### Task 7: Unit tests (AC: all)
- [x] 7.1 / 7.2 / 7.3: `sender_source_override_rewrites_records` verifies paper/live/replay tagging round-trips through the journal.
- [x] 7.4: `trades_by_source_returns_only_matching_rows` asserts the WHERE filter in isolation.
- [x] 7.5: `pnl_summary_known_dataset` validates net P&L, wins, losses, win rate, max drawdown over a hand-computed scenario; `pnl_summary_empty_when_no_trades` covers the empty case.
- [x] 7.6: `trace_decision_returns_all_rows_for_id` covers the join; non-existent id returns empty trace.
- [x] 7.7: `trade_record_round_trips_all_fields` asserts every field round-trips through SQLite without data loss.
- [x] 7.8: `readiness_500_trades_completes_with_finite_values` exercises the 500-trade dataset and confirms `meets_minimum_threshold(500)` passes.

### Task 8: Integration test (AC: end-to-end paper recording)
- [x] 8.1 + 8.2: `end_to_end_paper_trades_persist_with_paper_source` drives the orchestrator + journal worker end-to-end and asserts paper-source recording.
- [x] 8.3: `end_to_end_pnl_summary_matches_synthetic_history` verifies the summary against a hand-computed scenario.
- [x] 8.4: `end_to_end_decision_id_traceability` verifies trace_decision returns rows for the queried id only.

## Dev Notes

### Architecture Patterns & Constraints
- The source tag is set at the BrokerAdapter level, not in the event loop or journal. This maintains the principle that the event loop is adapter-agnostic.
- `TradeSource` enum should live in core crate since it is used by both engine (journal) and testkit (MockBrokerAdapter).
- Journal tables use the same schema for all sources — the source column is the ONLY difference. This means the same SQL queries, same analysis tools, same export formats work for paper and live data.
- `decision_id` is the primary traceability key (NFR17). Every trade decision generates a unique decision_id that flows through the entire pipeline. This enables post-hoc analysis: "why did the system take this trade?"
- The validation progression (unit tests -> replay -> paper -> live) is a core operational principle. Paper trading is the penultimate gate before live capital. The journal must contain enough data to make this gate meaningful.
- P&L calculations for paper trades use the same fee model as live — fees are real even if fills are simulated. This prevents paper P&L from being unrealistically optimistic.

### Project Structure Notes
```
crates/core/
└── src/
    └── types/
        └── trade_source.rs      (TradeSource enum: Live, Paper, Replay)
crates/engine/
├── src/
│   └── persistence/
│       ├── journal.rs           (source column added to tables and inserts)
│       └── query.rs             (JournalQuery, PnlSummary, DecisionTrace, ReadinessReport)
crates/testkit/
└── src/
    └── mock_broker.rs           (MockBrokerAdapter tags fills with TradeSource)
```

### References
- Architecture document: `_bmad-output/planning-artifacts/architecture.md` — Event journal, decision_id traceability
- Epics document: `_bmad-output/planning-artifacts/epics.md` — Epic 7, Story 7.4
- Dependencies: rusqlite (journal queries), serde (TradeSource serialization)
- Depends on: Story 4.1 (SQLite Event Journal), Story 7.1 (MockBrokerAdapter), Story 7.3 (Paper trading mode)
- NFR17: Full event traceability via decision_id
- FR32: Paper trade result recording with live fidelity

## Dev Agent Record

### Agent Model Used
- Claude Opus 4.7 (1M context) via the `bmad-dev-story` skill (2026-05-01).

### Debug Log References
- `cargo build --workspace` — clean (warnings only the upstream nightly-only fmt suggestions).
- `cargo clippy --workspace --all-targets` — clean (no warnings, no errors).
- `cargo fmt --all -- --check` — clean.
- `cargo test --workspace` — 35 + 93 + 5 + 5 + 2 + 325 + 0 + 16 + 11 + 12 + 12 + 4 + 9 + 7 + 3 + 5 + 6 + 16 + 15 = 581 tests, all passing. Notable counts: engine lib went from 321 → 325 (+4 journal tests for source column / migration / sender override) plus the new `query` submodule (10 tests) — `sender_source_override_rewrites_records` is double-counted between the two for clarity. The new integration suite `tests/paper_trade_recording.rs` adds 9 tests.

### Completion Notes List
- **Source-tag stamping seam.** Per Story 7.4 Task 2, the source tag is set "at the BrokerAdapter level" — pragmatically, this lands as a single override on `JournalSender`. Paper/replay orchestrators clone+stamp the supplied sender on `attach_journal()`; downstream callers (order manager, bracket manager, future risk subsystem) keep emitting records with `source: TradeSource::default()` and the sender rewrites them on the way out. This avoids touching every existing call site and centralises the per-mode behaviour at the orchestrator boundary.
- **MockBrokerAdapter source field.** The mock adapter now carries a `source: TradeSource` constructed via `MockBrokerAdapter::with_source(behavior, TradeSource)`. This is informational — the mock does not write to the journal directly — but it surfaces the per-mode tag at the wiring level so a code reviewer can verify by inspection that paper passes `Paper` and replay passes `Replay`.
- **Migration of pre-7.4 journals.** `EventJournal::new` runs an idempotent `ALTER TABLE … ADD COLUMN source TEXT NOT NULL DEFAULT 'live'` on tables that lack the column, then creates the matching indexes. Existing journals on disk continue to read as live data without manual intervention. Test `migration_adds_source_column_to_pre_story_7_4_journal` covers the upgrade path explicitly.
- **`TradeSource::parse`.** The natural API name `from_str` collides with `std::str::FromStr::from_str` (clippy `wrong_self_convention`-style warning). Renamed to `parse` to keep the "unknown variant ⇒ None" semantic without committing to the `FromStr` trait's `Err` machinery.
- **Fees.** Story 7.4 Task 3.3 mentions fee-aware P&L, but no fee model is wired into the journal write path today; the comment in the readiness report acknowledges this. The `pnl_summary` and `paper_readiness_report` use raw FixedPrice arithmetic (entry vs exit × qty). Fee inclusion is a downstream story (live execution wiring).
- **Readiness summary log.** `PaperTradingOrchestrator::emit_readiness_summary(&Connection)` is the Task 6.3 hook: caller passes a read connection at session end, the orchestrator logs the summary at `info` level and writes a `paper_readiness_summary` SystemEvent to the journal. The connection is parameterised because the orchestrator does not own the journal worker's `Connection` (single-thread pin invariant).

### File List
- **Added:** `crates/core/src/types/trade_source.rs` — `TradeSource` enum, `as_str`/`parse`/`Display` helpers, serde + tests.
- **Added:** `crates/engine/src/persistence/query.rs` — `JournalQuery`, `TradeRecord`, `OrderStateRecord`, `PnlSummary`, `DecisionTrace`, `ReadinessReport`, plus 10 unit tests.
- **Added:** `crates/engine/tests/paper_trade_recording.rs` — 9 end-to-end integration tests covering Tasks 8.1–8.4 and the source-tag invariants.
- **Modified:** `crates/core/src/types/mod.rs` — registered `trade_source` module and re-exported `TradeSource`.
- **Modified:** `crates/core/src/lib.rs` — re-exported `TradeSource` at crate root.
- **Modified:** `crates/engine/src/persistence/journal.rs` — source columns + indexes on `trade_events`/`order_states`, migration helper, `source` field on `TradeEventRecord`/`OrderStateChangeRecord`, `JournalSender::with_source` + `source_override`, `EventJournal::channel_with_source`, plus 4 new tests.
- **Modified:** `crates/engine/src/persistence/mod.rs` — registered `query` module and re-exported `JournalQuery`/`PnlSummary`/`DecisionTrace`/`ReadinessReport`/`TradeRecord`/`OrderStateRecord`.
- **Modified:** `crates/engine/src/paper/orchestrator.rs` — `attach_journal` re-tags the sender with `Paper`, `journal_sender()` accessor, `emit_readiness_summary(&Connection)` for Task 6.3.
- **Modified:** `crates/engine/src/paper/mod.rs` — re-exported the query types under the paper module for ergonomic access.
- **Modified:** `crates/engine/src/replay/orchestrator.rs` — `attach_journal` re-tags the sender with `Replay`, `MockBrokerAdapter::with_source(_, Replay)` at construction.
- **Modified:** `crates/engine/src/order_manager/mod.rs` + `crates/engine/src/order_manager/bracket.rs` — populate `source: TradeSource::default()` on `OrderStateChangeRecord`/`TradeEventRecord` literals (sender override does the actual stamping; the field exists so the struct compiles).
- **Modified:** `crates/engine/tests/paper_trading.rs` — updated existing `TradeEventRecord` literal to set `source` field.
- **Modified:** `crates/testkit/src/mock_broker.rs` — `MockBrokerAdapter::with_source(behavior, TradeSource)` constructor; `source()` accessor.
- **Modified:** `_bmad-output/implementation-artifacts/sprint-status.yaml` — flipped 7-4 from `ready-for-dev` to `review`.

### Change Log
- 2026-05-01 — Story 7.4 implemented end-to-end (TradeSource enum, journal source column + migration + indexes, JournalQuery / PnlSummary / DecisionTrace / ReadinessReport, paper & replay orchestrator re-tagging, integration suite). Build / clippy / fmt / tests all clean.
