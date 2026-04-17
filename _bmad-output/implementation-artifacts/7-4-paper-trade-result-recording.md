# Story 7.4: Paper Trade Result Recording

Status: ready-for-dev

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
- 1.1: Add `source TEXT NOT NULL DEFAULT 'live'` column to `trade_events` table in `crates/engine/src/persistence/journal.rs` — values: `'paper'`, `'live'`, `'replay'`
- 1.2: Add `source TEXT NOT NULL DEFAULT 'live'` column to `order_states` table
- 1.3: Update all CREATE TABLE statements to include the source column
- 1.4: Update all INSERT statements to include source value
- 1.5: Add index on source column for efficient filtering: `CREATE INDEX IF NOT EXISTS idx_trade_events_source ON trade_events(source)`

### Task 2: Propagate source tag through event pipeline (AC: paper fills tagged correctly)
- 2.1: Add `source: TradeSource` field to `EngineEvent` enum variants that record trade/order data — define `enum TradeSource { Live, Paper, Replay }` in core types
- 2.2: Set `TradeSource` at the BrokerAdapter level: `MockBrokerAdapter` tags fills with `Paper` (or `Replay` when used in replay mode), `RithmicAdapter` tags fills with `Live`
- 2.3: Determine paper vs replay context: pass `TradeSource` into `MockBrokerAdapter` at construction time — paper mode passes `Paper`, replay mode passes `Replay`
- 2.4: Journal writer reads `TradeSource` from `EngineEvent` and writes corresponding string to the source column

### Task 3: Ensure all fields match live recording fidelity (AC: order_id, fill_price, fill_size, timestamp, side, decision_id, P&L)
- 3.1: Verify `MockBrokerAdapter` fills include all required fields: `order_id` (generated), `fill_price` (FixedPrice from market), `fill_size` (u32), `timestamp` (UnixNanos from clock), `side` (Buy/Sell)
- 3.2: Verify `decision_id` flows from signal composite evaluation through order submission to fill event — the causal chain must be unbroken
- 3.3: Verify P&L is computed for paper trades exactly as for live: entry price, exit price, size, fees, net P&L — all in FixedPrice
- 3.4: Write a comparison test: generate a trade through MockBrokerAdapter and through a mock RithmicAdapter path, verify journal records have identical field coverage (differing only in source tag)

### Task 4: Implement journal query helpers for performance analysis (AC: P&L, win rate, max drawdown, trade count computable)
- 4.1: Implement `JournalQuery::trades_by_source(source: TradeSource) -> Vec<TradeRecord>` — query trade_events filtered by source
- 4.2: Implement `JournalQuery::pnl_summary(source: TradeSource) -> PnlSummary` struct with fields: `net_pnl: FixedPrice`, `win_count: u32`, `loss_count: u32`, `win_rate: f64`, `max_drawdown: FixedPrice`, `trade_count: u32`
- 4.3: Implement `JournalQuery::trade_count(source: TradeSource) -> u32` — efficient COUNT query
- 4.4: All query functions use parameterized SQL to filter by source
- 4.5: Create `crates/engine/src/persistence/query.rs` for these query functions (keep journal.rs focused on writing)

### Task 5: Implement decision_id traceability chain (AC: all evidence traceable via decision_id)
- 5.1: Verify decision_id is generated at signal composite evaluation and attached to the resulting trade decision
- 5.2: Verify decision_id is carried through: composite signal -> trade decision -> order submission -> fill event -> P&L calculation -> journal write
- 5.3: Implement `JournalQuery::trace_decision(decision_id: u64) -> DecisionTrace` that returns all journal entries (signals, order states, fills, P&L) sharing a decision_id
- 5.4: `DecisionTrace` provides the complete audit trail for a single trade decision — useful for post-analysis of paper trade outcomes

### Task 6: Implement go/no-go readiness query (AC: sufficient data for deployment decision)
- 6.1: Implement `JournalQuery::paper_readiness_report() -> ReadinessReport` that computes: total paper trades, positive expectancy (bool), win rate, profit factor, max drawdown, Sharpe estimate, consecutive loss max
- 6.2: `ReadinessReport` includes a `meets_minimum_threshold(min_trades: u32) -> bool` that checks trade count >= threshold (default 500)
- 6.3: Log readiness summary at `info` level when paper trading session ends
- 6.4: This is informational — the go/no-go decision is human-made, the system only provides data

### Task 7: Unit tests (AC: all)
- 7.1: Test that paper fills are written to journal with `source = 'paper'`
- 7.2: Test that live fills are written with `source = 'live'`
- 7.3: Test that replay fills are written with `source = 'replay'`
- 7.4: Test `trades_by_source` correctly filters by source tag
- 7.5: Test `pnl_summary` computes correct values from known trade data
- 7.6: Test `trace_decision` returns all events for a given decision_id
- 7.7: Test all fields (order_id, fill_price, fill_size, timestamp, side, decision_id, P&L) are present and correct in paper trade records
- 7.8: Test `paper_readiness_report` with a dataset of 500+ simulated trades

### Task 8: Integration test (AC: end-to-end paper recording)
- 8.1: Run paper trading with test data, generate multiple trades
- 8.2: Query journal and verify paper trades are recorded with correct source tag
- 8.3: Verify P&L summary matches expected values
- 8.4: Verify decision_id traceability across the full chain

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
### Debug Log References
### Completion Notes List
### File List
