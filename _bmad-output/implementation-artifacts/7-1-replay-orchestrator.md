# Story 7.1: Replay Orchestrator

Status: ready-for-dev

## Story

As a trader-operator,
I want to replay historical data through the exact same engine code path as live trading,
So that I can validate strategy logic against known data.

## Acceptance Criteria (BDD)

- Given `engine/src/replay/mod.rs` When engine started with `--replay <parquet-path>` Then wires SimClock instead of SystemClock, MockBrokerAdapter instead of RithmicAdapter, Parquet data source feeds MarketEvent into same SPSC buffer, event loop code identical (no replay-specific branching)
- Given replay running When SimClock advances Then time based on recorded event timestamps (synthetic monotonic), no real-time waiting, targeting >=100x real-time (NFR5)
- Given MockBrokerAdapter in replay When orders submitted Then fills simulated (immediate fill at market price V1 default), simulated fills via same FillEvent SPSC queue
- Given replay completes When all data processed Then summary output: total events, trades, P&L, win rate, max drawdown, all events written to journal

## Tasks / Subtasks

### Task 1: Create replay module structure (AC: engine/src/replay/mod.rs)
- 1.1: Create `crates/engine/src/replay/mod.rs` with `pub mod data_source;` and the `ReplayOrchestrator` struct
- 1.2: Update `crates/engine/src/lib.rs` to declare `pub mod replay`
- 1.3: Define `ReplayConfig` struct with fields: `parquet_path: PathBuf`, `fill_model: FillModel` (enum: ImmediateAtMarket as V1 default), `summary_output: bool`

### Task 2: Implement Parquet data source for replay input (AC: Parquet data source feeds MarketEvent into SPSC)
- 2.1: Create `crates/engine/src/replay/data_source.rs` with `ParquetReplaySource` struct
- 2.2: Implement `ParquetReplaySource::open(path: &Path) -> Result<Self>` that opens a Parquet file via `parquet` crate reader
- 2.3: Implement `Iterator<Item = MarketEvent>` for `ParquetReplaySource` — reads rows sequentially, converts Parquet columns (timestamp i64 nanos, price i64 quarter-ticks, size u32, side, event_type, symbol_id) into `MarketEvent`
- 2.4: Support multi-file replay: accept a directory path, sort files by name (date-partitioned), iterate through files in order
- 2.5: Validate Parquet schema on open — return clear error if schema does not match expected columns

### Task 3: Implement ReplayOrchestrator wiring (AC: SimClock, MockBrokerAdapter, same SPSC, no branching)
- 3.1: Implement `ReplayOrchestrator::new(config: ReplayConfig) -> Result<Self>` that creates: `SimClock` (from testkit), `MockBrokerAdapter` (from testkit, configured with fill model), `ParquetReplaySource`
- 3.2: Wire `ParquetReplaySource` output into the same SPSC buffer used by live data path — the data source pushes `MarketEvent` into the producer end of the existing SPSC channel
- 3.3: Wire `MockBrokerAdapter` fill output into the same FillEvent SPSC queue used by the live fill path — no separate replay fill handling
- 3.4: Inject `SimClock` as the `Clock` impl — all time-dependent code (signals, risk, regime) reads time from this clock
- 3.5: Ensure NO replay-specific branching exists in event_loop.rs — the event loop processes events identically regardless of source

### Task 4: Implement SimClock time advancement from recorded timestamps (AC: synthetic monotonic, no real-time waiting)
- 4.1: Before each `MarketEvent` is pushed into the SPSC buffer, call `sim_clock.set_time(event.timestamp)` to advance the clock to the event's recorded timestamp
- 4.2: Ensure timestamps are monotonically increasing — if a recorded timestamp is earlier than current SimClock time, log warning and skip advancement (data quality issue)
- 4.3: No `sleep()` or real-time pacing between events — events are processed as fast as the engine can consume them
- 4.4: Log elapsed wall-clock time vs simulated time at completion to report replay speed multiple (e.g., "Replayed 6.5h of data in 3.2s = 7312x real-time")

### Task 5: Implement MockBrokerAdapter fill simulation (AC: immediate fill at market price, FillEvent via same queue)
- 5.1: Configure `MockBrokerAdapter` with `FillModel::ImmediateAtMarket` — when `submit_order()` is called, immediately generate a `FillEvent` at the current best bid/ask (depending on order side)
- 5.2: Push the generated `FillEvent` into the same SPSC queue that live Rithmic fills use — the event loop handles it identically
- 5.3: Assign unique `fill_id` to each simulated fill
- 5.4: `MockBrokerAdapter` must implement the full `BrokerAdapter` trait — `subscribe()`, `cancel_order()`, `query_positions()` also need sensible replay behavior (subscribe=no-op, cancel=acknowledge, query_positions=return tracked state)

### Task 6: Implement CLI flag and startup wiring (AC: --replay CLI flag)
- 6.1: Add `--replay <parquet-path>` CLI argument to the engine binary (clap)
- 6.2: In `main.rs`, detect `--replay` flag and branch at startup ONLY (not in hot path): construct `ReplayOrchestrator` instead of connecting to Rithmic
- 6.3: All downstream code (event loop, signals, risk, regime, order manager, journal) receives trait objects and is unaware of replay vs live
- 6.4: Example CLI: `cargo run -p engine -- --replay data/market/MES/2026-04-15.parquet --config config/paper.toml`

### Task 7: Implement replay completion summary (AC: summary output on completion)
- 7.1: When `ParquetReplaySource` exhausts all events, signal replay completion to the orchestrator
- 7.2: Compute and output summary: total events processed, total trades taken, net P&L (FixedPrice), win rate (%), max drawdown (FixedPrice), elapsed wall-clock time, replay speed multiple
- 7.3: All events during replay are written to the event journal (Story 4.1) identically to live — the journal does not know it is recording replay data
- 7.4: Output summary to stdout and log at `info` level

### Task 8: Unit tests (AC: all)
- 8.1: Test `ParquetReplaySource` reads a test Parquet file and produces correct `MarketEvent` sequence
- 8.2: Test `ReplayOrchestrator` wiring — SimClock, MockBrokerAdapter, and data source are correctly connected
- 8.3: Test SimClock advances monotonically based on event timestamps
- 8.4: Test MockBrokerAdapter produces FillEvent for submitted orders
- 8.5: Test replay completion summary is computed correctly from known data
- 8.6: Test multi-file replay processes files in date order

### Task 9: Integration test (AC: end-to-end replay)
- 9.1: Create a small test Parquet file with known market data (use testkit market_gen)
- 9.2: Run full replay through the engine event loop
- 9.3: Verify journal contains expected events
- 9.4: Verify summary output matches expected values

## Dev Notes

### Architecture Patterns & Constraints
- The replay orchestrator is a STARTUP-TIME concern only. Once the event loop is running, the code path is identical to live. The only difference is which implementations of Clock, BrokerAdapter, and data source are injected.
- `SimClock` lives in testkit crate (already planned in Story 1.5). It implements the `Clock` trait and provides `set_time(nanos: UnixNanos)` for replay advancement.
- `MockBrokerAdapter` lives in testkit crate (already planned in Story 1.5). It implements the `BrokerAdapter` trait with configurable fill behavior.
- The SPSC buffer is the same type used for live data — `ParquetReplaySource` pushes events into the producer end. The event loop consumes from the consumer end identically.
- NO `if replay { ... }` branches in event_loop.rs. This is enforced by the trait-based architecture: the event loop operates on `dyn Clock`, `dyn BrokerAdapter`, and receives `MarketEvent` from a generic SPSC consumer.
- Replay fidelity boundaries (documented in architecture): replay CANNOT model queue position, partial fills, market impact, or liquidity variation. It validates logic correctness, not P&L prediction.

### Project Structure Notes
```
crates/engine/
├── src/
│   ├── lib.rs
│   ├── main.rs                  (--replay flag detection, startup wiring)
│   └── replay/
│       ├── mod.rs               (ReplayOrchestrator, ReplayConfig)
│       └── data_source.rs       (ParquetReplaySource)
crates/testkit/
├── src/
│   ├── sim_clock.rs             (SimClock — already planned)
│   └── mock_broker.rs           (MockBrokerAdapter — already planned)
config/
└── paper.toml                   (paper trading config, used by replay)
```

### References
- Architecture document: `_bmad-output/planning-artifacts/architecture.md` — Replay, Clock abstraction, BrokerAdapter trait
- Epics document: `_bmad-output/planning-artifacts/epics.md` — Epic 7, Story 7.1
- Dependencies: parquet (arrow-rs), clap, crossbeam-channel, tokio, tracing
- Depends on: Story 1.5 (Clock trait, SimClock, MockBrokerAdapter), Story 2.4 (Parquet format), Story 4.1 (Event journal)
- NFR5: >=100x real-time replay throughput
- NFR15: Deterministic replay (supported by this story, verified by Story 7.2)
- FR29: Historical replay through live code path

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
