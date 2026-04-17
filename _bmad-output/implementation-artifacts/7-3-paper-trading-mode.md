# Story 7.3: Paper Trading Mode

Status: ready-for-dev

## Story

As a trader-operator,
I want to run against live market data without submitting real orders,
So that I can validate the system in real market conditions before risking capital.

## Acceptance Criteria (BDD)

- Given engine started with `--config config/paper.toml` When paper mode active Then live data flows from Rithmic through real path, trade decisions generated normally, orders routed to MockBrokerAdapter instead of real OrderPlant, MockBrokerAdapter simulates fills (immediate at market price V1)
- Given paper trading running When operates Then SystemClock used (real time), all circuit breakers/risk/regime function normally, only difference is order routing destination
- Given paper vs live When config compared Then switch is single config parameter (broker mode: paper vs live), no code path differences beyond BrokerAdapter impl injected at startup

## Tasks / Subtasks

### Task 1: Define broker mode config parameter (AC: single config parameter switches mode)
- 1.1: Add `broker_mode` field to the engine config struct: `enum BrokerMode { Live, Paper }` in `crates/engine/src/config/` (or `crates/core/src/config/` depending on where config types live)
- 1.2: In `config/paper.toml`, set `broker_mode = "paper"` — this is the ONLY difference from `config/live.toml` for broker behavior
- 1.3: In `config/live.toml`, set `broker_mode = "live"`
- 1.4: Implement `Deserialize` for `BrokerMode` via serde

### Task 2: Implement startup-time BrokerAdapter selection (AC: injected at startup, no code branching)
- 2.1: In `crates/engine/src/main.rs` (or startup module), read `broker_mode` from loaded config
- 2.2: When `BrokerMode::Paper`: instantiate `MockBrokerAdapter` from testkit, configure with `FillModel::ImmediateAtMarket`
- 2.3: When `BrokerMode::Live`: instantiate `RithmicAdapter` from broker crate (existing behavior)
- 2.4: Pass the selected `Box<dyn BrokerAdapter>` (or generic) into the event loop — all downstream code is adapter-agnostic
- 2.5: This is the ONLY place where `BrokerMode` is checked — no other code inspects this value

### Task 3: Wire live Rithmic data with MockBrokerAdapter (AC: live data, simulated execution)
- 3.1: In paper mode, connect to Rithmic TickerPlant for live market data (same as live mode)
- 3.2: Do NOT connect to Rithmic OrderPlant — orders go to MockBrokerAdapter instead
- 3.3: Market data flows through the real SPSC path: Rithmic -> MarketEvent -> SPSC -> OrderBook -> Signals -> Regime -> Decision
- 3.4: When a trade decision fires, the order is submitted to MockBrokerAdapter via the same `BrokerAdapter::submit_order()` interface
- 3.5: MockBrokerAdapter generates FillEvent and pushes it into the same FillEvent SPSC queue

### Task 4: Ensure SystemClock is used in paper mode (AC: real time, not SimClock)
- 4.1: Paper mode uses `SystemClock` (not `SimClock`) because market data is live and timestamps are real
- 4.2: Verify that clock selection is independent of broker mode: replay uses SimClock (because data is historical), paper uses SystemClock (because data is live), live uses SystemClock
- 4.3: Document the clock/broker matrix: replay = SimClock + MockBroker, paper = SystemClock + MockBroker, live = SystemClock + RithmicAdapter

### Task 5: Verify all risk/safety systems function in paper mode (AC: circuit breakers, risk, regime all normal)
- 5.1: Verify circuit breakers are armed and functional — daily loss limit, consecutive loss limit, max position size all enforced
- 5.2: Verify regime detection operates on live data normally
- 5.3: Verify fee calculations and fee-gating function normally
- 5.4: The only relaxation: paper mode may optionally log a startup banner "PAPER TRADING MODE" at `warn` level to make mode visible in logs

### Task 6: Add paper mode startup logging and validation (AC: clear mode identification)
- 6.1: On startup in paper mode, log at `warn` level: "Starting in PAPER TRADING mode — orders will NOT be sent to exchange"
- 6.2: Validate that paper.toml does not contain live credentials — if `broker_mode = "paper"` but Rithmic order credentials are present, log a warning (not an error — credentials may exist in env vars for data access)
- 6.3: Log the broker adapter type at startup: "BrokerAdapter: MockBrokerAdapter (paper)" or "BrokerAdapter: RithmicAdapter (live)"

### Task 7: Unit tests (AC: all)
- 7.1: Test that `BrokerMode::Paper` config deserializes correctly from TOML
- 7.2: Test that paper mode instantiates `MockBrokerAdapter`, not `RithmicAdapter`
- 7.3: Test that paper mode uses `SystemClock`, not `SimClock`
- 7.4: Test that `MockBrokerAdapter` in paper mode produces fills for submitted orders via the FillEvent queue
- 7.5: Test that circuit breakers function identically in paper mode (submit enough losing trades to trip daily loss limit)

### Task 8: Integration test (AC: end-to-end paper mode)
- 8.1: Create integration test that starts engine in paper mode with mock Rithmic data source (testkit market_gen feeding the SPSC as if it were Rithmic)
- 8.2: Verify trade decisions are generated from signals
- 8.3: Verify orders go to MockBrokerAdapter and fills are received
- 8.4: Verify journal records the trades (Story 7.4 covers source tagging)

## Dev Notes

### Architecture Patterns & Constraints
- Paper trading is architecturally trivial by design: the `BrokerAdapter` trait abstraction means paper mode is just a different impl injected at startup. No feature flags, no conditional compilation, no runtime branching in the hot path.
- The clock/broker mode matrix:
  | Mode   | Clock       | BrokerAdapter      | Data Source      |
  |--------|-------------|--------------------|------------------|
  | Live   | SystemClock | RithmicAdapter     | Rithmic live     |
  | Paper  | SystemClock | MockBrokerAdapter  | Rithmic live     |
  | Replay | SimClock    | MockBrokerAdapter  | Parquet files    |
- Paper mode still connects to Rithmic for market data (TickerPlant) but does NOT connect to the OrderPlant. This means Rithmic credentials for data access are still required.
- MockBrokerAdapter in paper mode uses `FillModel::ImmediateAtMarket` (V1 default). Future versions may add slippage models, partial fills, or latency simulation.
- Watchdog is deferred to live-trading preparation per architecture decision. Paper trading has no capital at risk.

### Project Structure Notes
```
crates/engine/
├── src/
│   ├── main.rs                  (BrokerMode check at startup ONLY)
│   └── config/
│       └── loader.rs            (BrokerMode deserialization)
config/
├── default.toml                 (shared defaults)
├── paper.toml                   (broker_mode = "paper")
└── live.toml                    (broker_mode = "live")
crates/testkit/
└── src/
    └── mock_broker.rs           (MockBrokerAdapter — shared with replay)
```

### References
- Architecture document: `_bmad-output/planning-artifacts/architecture.md` — BrokerAdapter trait, broker isolation decision
- Epics document: `_bmad-output/planning-artifacts/epics.md` — Epic 7, Story 7.3
- Dependencies: testkit (MockBrokerAdapter), broker (RithmicAdapter), config crate, serde
- Depends on: Story 1.5 (BrokerAdapter trait, MockBrokerAdapter), Story 2.1 (Rithmic connection for data), Story 7.1 (MockBrokerAdapter fill model)
- FR31: Paper trading mode — live data, simulated execution

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
