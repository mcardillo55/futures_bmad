---
stepsCompleted: ['step-01-validate-prerequisites', 'step-02-design-epics', 'step-03-create-stories', 'step-04-final-validation']
inputDocuments:
  - prd.md
  - architecture.md
  - brainstorming/brainstorming-session-2026-04-12-001.md
---

# futures_bmad - Epic Breakdown

## Overview

This document provides the complete epic and story breakdown for futures_bmad, decomposing the requirements from the PRD, Architecture, and Brainstorming into implementable stories.

## Requirements Inventory

### Functional Requirements

FR1: System can connect to a broker's market data feed and receive real-time tick and Level 2 order book data for configured CME equity index futures contracts
FR2: System can reconstruct and maintain a current order book state from streaming market data updates
FR3: System can ingest historical market data from Parquet files for replay processing
FR4: System can detect and flag market data gaps or quality degradation during live operation
FR5: System can record market data continuously during market hours, regardless of whether trading strategies are active
FR6: System can compute order book imbalance (OBI) from current order book state
FR7: System can compute volume-synchronized probability of informed trading (VPIN) from trade flow data
FR8: System can compute microprice from order book state
FR9: System can identify structural price levels from market data (prior day high/low, volume point of control, key reaction levels)
FR10: System can evaluate composite signals at structural price levels and generate trade/no-trade decisions
FR11: System can gate trade decisions based on fee-aware minimum edge threshold (expected edge must exceed 2x total round-trip costs)
FR12: System can submit market orders to the exchange via broker API
FR13: System can submit atomic bracket orders (entry + take-profit + stop-loss) with protective stops resting at the exchange
FR14: System can cancel open orders
FR15: System can track order state transitions (submitted, filled, partially filled, cancelled, rejected) from broker confirmations
FR16: System can reconcile local position state with broker-reported position state
FR17: System can enforce configurable position size limits (max contracts per position)
FR18: System can enforce configurable daily loss limits and halt trading when exceeded
FR19: System can track consecutive losses and halt trading when a configured threshold is exceeded
FR20: System can detect anomalous positions (positions existing outside active strategy context) and automatically flatten them
FR21: System can send heartbeats to an independent watchdog process at a configurable interval
FR22: Watchdog can detect missed heartbeats and autonomously flatten all positions via its own broker connection
FR23: Watchdog can send operator alerts (email/push notification) when activated
FR24: System can alert the operator on circuit breaker events
FR25: System can respect configurable event-aware trading windows (e.g., FOMC, CPI, NFP) by disabling strategies or reducing exposure during scheduled high-impact events
FR26: System can classify current market regime (trending, rotational, volatile) from market data
FR27: System can enable or disable trading strategies based on current regime classification
FR28: System can detect and log regime transitions during a trading session
FR29: System can replay historical market data through the same code path used for live trading
FR30: System can produce deterministic, reproducible results when replaying the same historical data
FR31: System can operate in paper trading mode — processing live market data and generating signals/decisions without submitting real orders
FR32: System can record paper trade results with the same fidelity as live trade results
FR33: System can log all trade events, order state transitions, and P&L records to a structured event journal (SQLite)
FR34: System can store market data (ticks, L2 snapshots) in columnar format (Parquet) for long-term retention
FR35: System can log circuit breaker events, regime transitions, and system state changes with full diagnostic context
FR36: System can produce structured logs suitable for real-time monitoring via log tailing
FR37: System can log trade data in a format suitable for Section 1256 tax reporting (realized P&L per contract, per day)
FR38: Operator can configure strategy parameters, risk limits, regime thresholds, and broker connectivity via configuration files
FR39: System can execute a startup sequence (connect, verify, sync positions, arm safety systems, begin operation)
FR40: System can execute a graceful shutdown sequence (disable strategies, cancel orders, verify flat, disconnect, flush logs)
FR41: System can recover from broker disconnections and resynchronize state on reconnect
FR42: Operator can deploy updated system binaries and restart the service without data loss

### NonFunctional Requirements

NFR1: Tick-to-decision latency within the budget required by the active trading strategy — architecture supports sub-millisecond processing to preserve optionality for latency-sensitive strategies
NFR2: Zero heap allocations on the hot path during steady-state operation — all data structures pre-allocated at startup
NFR3: No garbage collection pauses, lock contention, or context switches on the hot path thread
NFR4: Market data ingestion keeps pace with broker feed rates without dropping messages under normal market conditions
NFR5: Historical replay processes every tick in sequence through the same code path as live trading, with no sampling or skipping — targeting at minimum 100x real-time throughput
NFR6: System uptime >99.5% during CME regular trading hours (8:30 AM - 3:00 PM CT)
NFR7: Broker disconnections detected within 5 seconds and reconnection attempted automatically with position state resynchronization
NFR8: Watchdog detects primary system failure within 30 seconds (configurable heartbeat threshold) and flattens positions autonomously
NFR9: No data loss on unclean shutdown — event journal durable (SQLite WAL mode), in-flight state recoverable on restart
NFR10: Circuit breakers activate within one tick of detecting a violation condition — no delayed enforcement
NFR11: Broker API credentials not stored in plaintext in configuration files or source control — environment variables or encrypted config at minimum
NFR12: VPS access restricted to SSH key authentication only — no password authentication
NFR13: Watchdog-to-primary communication authenticated to prevent unauthorized kill signals
NFR14: No broker credentials or trading data transmitted over unencrypted channels
NFR15: Historical replay produces bit-identical results on identical input data — full determinism required for signal validation
NFR16: Position state always reconcilable with broker-reported state — any mismatch triggers circuit breaker, not silent correction
NFR17: Event journal maintains referential integrity — every trade event traceable from signal through execution to P&L

### Additional Requirements

- Starter template: Custom Cargo virtual workspace (4 crates: core, broker, engine, testkit) — no existing starter/framework applies
- Rust 1.95.0 stable, Edition 2024, target x86_64-unknown-linux-gnu
- FixedPrice(i64) quarter-tick representation for all prices — integer arithmetic only on hot path
- Fixed-array OrderBook with 10 levels, is_tradeable() validation before signal evaluation
- Signal trait with incremental update, reset(), snapshot() for determinism and replay
- Concrete SignalPipeline (V1) — OBI, VPIN, microprice as named fields, zero vtable overhead
- Order state machine with Uncertain (5s timeout) and PendingRecon states
- Atomic bracket orders with flatten retry (3 attempts, 1s interval) escalating to panic mode
- Tiered circuit breakers: gates (auto-clear) vs breakers (manual reset) — 10 distinct types
- SPSC buffer circuit-break strategy (50% warn / 80% disable trading / 95% full circuit break, 128K entries)
- Clock trait abstraction (SystemClock vs SimClock) — mandatory injection, no direct time access
- Order state durable persistence — write to SQLite WAL before transitioning to Submitted
- 5-state reconnection FSM (Connected, Disconnected, Reconnecting, Reconciling, Ready) with 60s timeout
- CI/CD via GitHub Actions (cargo fmt, clippy, nextest, audit)
- Prometheus metrics via HTTP endpoint (axum) for latency, buffer occupancy, P&L, connection state
- Watchdog deferred to live-trading preparation — paper trading has no capital at risk
- Zero-allocation verification via dhat crate in CI
- Fee schedule staleness gate: 30-day warning, 60-day block on trade evaluation
- Malformed message handling: skip individual, circuit-break on pattern (>10 in 60s sliding window)
- Signal validity guards: NaN/Inf/empty-book pre-condition checks on every computation
- Causality logging: every trade decision links signal values to composite score to order via decision_id
- Graceful degradation hierarchy: signal quality degrades, stop new trades, flatten existing, watchdog takes over

### UX Design Requirements

No UX Design document was provided. This system has no GUI — operator interaction is via SSH, log tailing, and configuration files (per PRD). No UX design requirements apply.

### FR Coverage Map

FR1: Epic 2 - Broker market data connection
FR2: Epic 2 - Order book reconstruction from streaming updates
FR3: Epic 2 - Historical Parquet ingestion for replay
FR4: Epic 2 - Data gap detection and quality flagging
FR5: Epic 2 - Continuous market data recording
FR6: Epic 3 - OBI computation from order book
FR7: Epic 3 - VPIN computation from trade flow
FR8: Epic 3 - Microprice computation from order book
FR9: Epic 3 - Structural price level identification
FR10: Epic 3 - Composite signal evaluation at structural levels
FR11: Epic 3 - Fee-aware minimum edge trade gating
FR12: Epic 4 - Market order submission via broker API
FR13: Epic 4 - Atomic bracket orders with exchange-resting stops
FR14: Epic 4 - Order cancellation
FR15: Epic 4 - Order state transition tracking
FR16: Epic 4 - Position reconciliation with broker
FR17: Epic 5 - Configurable position size limits
FR18: Epic 5 - Configurable daily loss limits
FR19: Epic 5 - Consecutive loss tracking and halt
FR20: Epic 5 - Anomalous position detection and flatten
FR21: Epic 9 - Watchdog heartbeat interface
FR22: Deferred - Watchdog autonomous flatten (post-V1, live trading preparation)
FR23: Deferred - Watchdog operator alerting (post-V1, live trading preparation)
FR24: Epic 5 - Circuit breaker operator alerting
FR25: Epic 5 - Event-aware trading windows (FOMC, CPI, NFP)
FR26: Epic 6 - Market regime classification
FR27: Epic 6 - Strategy enable/disable by regime
FR28: Epic 6 - Regime transition logging
FR29: Epic 7 - Historical replay through live code path
FR30: Epic 7 - Deterministic reproducible replay
FR31: Epic 7 - Paper trading mode
FR32: Epic 7 - Paper trade result recording
FR33: Epic 4 - SQLite event journal (WAL mode)
FR34: Epic 2 - Parquet market data storage
FR35: Epic 9 - Circuit breaker and regime diagnostic logging
FR36: Epic 9 - Structured logs for real-time monitoring
FR37: Epic 9 - Section 1256 tax reporting data
FR38: Epic 1 (config types/validation) + Epic 8 (full config loading)
FR39: Epic 8 - Startup sequence
FR40: Epic 8 - Graceful shutdown sequence
FR41: Epic 8 - Reconnection recovery with state resync
FR42: Epic 8 - Deploy updated binaries without data loss

Coverage: 40/42 FRs mapped. FR22, FR23 deferred per Architecture decision (watchdog binary built during live-trading preparation).

## Epic List

### Epic 1: Project Foundation & Core Domain
Set up the Cargo workspace, implement all foundational domain types (FixedPrice, UnixNanos, OrderBook, events, Clock), traits (Signal, RegimeDetector, BrokerAdapter, Clock), config structures, and the testkit crate with SimClock, MockBroker, and OrderBookBuilder.
**FRs covered:** Partial FR38 (config types/validation)

### Epic 2: Market Data Ingestion & Recording
Connect to Rithmic via rithmic-rs, receive live L1/L2 data, reconstruct order book from streaming updates, detect data gaps, record continuously to Parquet, and ingest historical Parquet files.
**FRs covered:** FR1, FR2, FR3, FR4, FR5, FR34

### Epic 3: Signal Analysis & Trade Decision Engine
Implement OBI, VPIN, and microprice signals. Build structural price level identification. Composite signal evaluation with weighted scoring. Fee-aware trade gating with staleness checks.
**FRs covered:** FR6, FR7, FR8, FR9, FR10, FR11

### Epic 4: Order Execution, Position Management & Event Journal
SQLite event journal (WAL mode) as the persistence foundation. Market order submission via Rithmic OrderPlant. Atomic bracket orders with exchange-resting stops. Order state machine with Uncertain/PendingRecon states. WAL persistence before submission. Position reconciliation with broker.
**FRs covered:** FR12, FR13, FR14, FR15, FR16, FR33

### Epic 5: Risk Management & Circuit Breakers
Tiered circuit breakers (gates vs breakers): position limits, daily loss, consecutive losses, max trades, anomalous position detection, buffer overflow, data quality, fee staleness, malformed messages. Panic mode with flatten retry escalation. Event-aware trading windows.
**FRs covered:** FR17, FR18, FR19, FR20, FR24, FR25

### Epic 6: Regime Detection & Strategy Orchestration
Threshold-based regime detection (trending, rotational, volatile, unknown). Strategy enable/disable based on regime state. Regime transition logging.
**FRs covered:** FR26, FR27, FR28

### Epic 7: Historical Replay & Paper Trading
Replay orchestrator wiring SimClock + MockBroker + Parquet data source through the same event loop as live. Deterministic replay verification. Paper trading mode (live data, simulated execution). Paper trade result recording.
**FRs covered:** FR29, FR30, FR31, FR32

### Epic 8: System Lifecycle & Reconnection
Startup sequence (connect, verify, replay WAL, sync positions, arm safety, begin). Graceful shutdown (disable, cancel, verify flat, disconnect, flush). 5-state reconnection FSM with mandatory reconciliation. Full config loading (layered TOML + env vars). Deploy workflow with rollback.
**FRs covered:** FR38 (full), FR39, FR40, FR41, FR42

### Epic 9: Observability & Monitoring
Structured logging with JSON output and causality tracing (decision_id). Circuit breaker and regime transition diagnostic logging. Tax reporting data format (Section 1256). Prometheus metrics endpoint (latency, buffer occupancy, P&L, connection state). Watchdog heartbeat interface. Credential management (secrecy crate, env vars).
**FRs covered:** FR21, FR35, FR36, FR37

## Epic 1: Project Foundation & Core Domain

Set up the Cargo workspace, implement all foundational domain types (FixedPrice, UnixNanos, OrderBook, events, Clock), traits (Signal, RegimeDetector, BrokerAdapter, Clock), config structures, and the testkit crate with SimClock, MockBroker, and OrderBookBuilder. Every subsequent epic builds on these types and traits.

### Story 1.1: Cargo Workspace & CI/CD Setup

As a developer,
I want a properly structured Cargo virtual workspace with all four crates scaffolded and CI running,
So that all subsequent development has a consistent, verified foundation to build on.

**Acceptance Criteria:**

**Given** an empty project directory
**When** the workspace is initialized
**Then** a Cargo virtual workspace exists with `crates/core`, `crates/broker`, `crates/engine`, `crates/testkit`
**And** `Cargo.toml` at root defines `workspace.members` and `workspace.dependencies` for all foundational crates (rithmic-rs =0.7.2, tokio 1.51.1, rtrb 0.3.3, rusqlite 0.38.0, tracing 0.1.44, thiserror 2.0.18, anyhow 1.0.100, proptest 1.9.0, insta 1.46.3, chrono 0.4.44, config 0.15.22, secrecy 0.10.3, tikv-jemallocator 0.6.1, core_affinity 0.8.3, crossbeam-channel 0.5.15, prost 0.14.3, tokio-tungstenite 0.29.0, databento =0.40.0, bumpalo 3.20.2)
**And** Rust edition is 2024, toolchain is 1.95.0 stable
**And** dependency direction is enforced: `engine` depends on `broker` and `core`; `broker` depends on `core`; `testkit` depends on `core`; `core` has zero internal crate dependencies
**And** `rustfmt.toml` and `clippy.toml` exist with project conventions
**And** `.gitignore` excludes `target/`, `.env`, `data/`, `*.db`, `*.db-wal`
**And** `.env.example` documents required environment variables (RITHMIC_USER, RITHMIC_PASSWORD, etc.)
**And** `.github/workflows/ci.yml` runs: `cargo fmt --check`, `cargo clippy -- -D warnings`, `cargo nextest run`, `cargo audit`
**And** `config/default.toml`, `config/paper.toml`, `config/live.toml` exist as placeholder config files
**And** `cargo nextest run --workspace` passes with zero tests (clean build)
**And** `cargo clippy --workspace -- -D warnings` passes

### Story 1.2: Core Price & Time Types

As a developer,
I want foundational price and time types with guaranteed correctness,
So that all price arithmetic uses integer-only FixedPrice with saturating behavior and all timestamps use nanosecond precision.

**Acceptance Criteria:**

**Given** the `core` crate
**When** `FixedPrice(i64)` is implemented
**Then** it represents prices in quarter-ticks (e.g., 4482.25 → 17929)
**And** it implements `saturating_add`, `saturating_sub`, `saturating_mul` — never panics on overflow
**And** it implements `Display` for human-readable format (e.g., "4482.25") — conversion to f64 only for display
**And** it implements `Debug`, `Clone`, `Copy`, `PartialEq`, `Eq`, `PartialOrd`, `Ord`, `Hash`
**And** it provides `from_f64(price: f64) -> FixedPrice` for config loading (rounds via banker's rounding)
**And** it provides `to_f64(&self) -> f64` explicitly marked as display-only

**Given** the `core` crate
**When** `UnixNanos(u64)` is implemented
**Then** it represents timestamps in nanosecond precision
**And** it implements `Debug`, `Clone`, `Copy`, `PartialEq`, `Eq`, `PartialOrd`, `Ord`
**And** it provides conversion to/from `chrono::DateTime` for display only

**Given** the `core` crate
**When** `Side` enum is implemented
**Then** it has variants `Buy` and `Sell`
**And** it implements `Debug`, `Clone`, `Copy`, `PartialEq`, `Eq`

**Given** the `core` crate
**When** `Bar` struct is implemented
**Then** it contains `open`, `high`, `low`, `close` as `FixedPrice`, `volume` as `u64`, and `timestamp` as `UnixNanos`

**Given** property tests exist for `FixedPrice`
**When** `proptest` runs arbitrary `i64` values through arithmetic operations
**Then** no operation panics (saturating behavior verified)
**And** `a.saturating_add(b).saturating_sub(b)` equals `a` for non-overflow cases
**And** `from_f64(price).to_f64()` round-trips correctly for valid price values

### Story 1.3: Order Book & Market Events

As a developer,
I want a zero-allocation order book and market event types,
So that market data can be processed on the hot path without heap allocations.

**Acceptance Criteria:**

**Given** the `core` crate
**When** `Level` struct is implemented
**Then** it contains `price: FixedPrice`, `size: u32`, `order_count: u16`
**And** it implements `Debug`, `Clone`, `Copy`

**Given** the `core` crate
**When** `OrderBook` struct is implemented
**Then** it contains `bids: [Level; 10]`, `asks: [Level; 10]`, `bid_count: u8`, `ask_count: u8`, `timestamp: UnixNanos`
**And** it is stack-allocated with no heap usage (fixed-size arrays)
**And** it provides `update_bid(index, level)` and `update_ask(index, level)` for in-place updates
**And** it provides `best_bid() -> Option<&Level>` and `best_ask() -> Option<&Level>`
**And** it provides `mid_price() -> Option<FixedPrice>`
**And** it provides `spread() -> Option<FixedPrice>`
**And** it provides `empty() -> OrderBook` constructor

**Given** `OrderBook::is_tradeable()` is implemented
**When** called on an order book
**Then** it returns `true` only when: `bid_count >= 3` AND `ask_count >= 3` AND spread is within `max_spread_threshold` AND top-level sizes are non-zero AND bids are descending AND asks are ascending
**And** the `max_spread_threshold` is passed as a parameter (per-instrument, from config)

**Given** the `core` crate
**When** `MarketEvent` is implemented
**Then** it contains `timestamp: UnixNanos`, `symbol_id: u32`, `event_type: MarketEventType`, `price: FixedPrice`, `size: u32`, `side: Option<Side>`
**And** it implements `Debug`, `Clone`, `Copy` (stack-allocated, no heap)
**And** `MarketEventType` enum includes variants for trade, bid update, ask update, and book snapshot

**Given** property tests exist for `OrderBook`
**When** `proptest` generates random order books
**Then** `is_tradeable()` returns `false` for books with fewer than 3 bid or ask levels
**And** `is_tradeable()` returns `false` for books with non-monotonic prices
**And** `best_bid()` always returns the highest bid price when `bid_count > 0`

### Story 1.4: Order & Position Types

As a developer,
I want complete order and position domain types,
So that order lifecycle tracking and position management have well-defined type-safe representations.

**Acceptance Criteria:**

**Given** the `core` crate
**When** `OrderState` enum is implemented
**Then** it includes variants: `Idle`, `Submitted`, `Confirmed`, `PartialFill`, `Filled`, `Rejected`, `Cancelled`, `PendingCancel`, `Uncertain`, `PendingRecon`, `Resolved`
**And** it implements `Debug`, `Clone`, `Copy`, `PartialEq`, `Eq`

**Given** the `core` crate
**When** `OrderParams` struct is implemented
**Then** it contains fields for symbol, side, quantity, order type (Market/Limit/Stop), and price (optional, for limit/stop)
**And** it implements `Debug`, `Clone`

**Given** the `core` crate
**When** `BracketOrder` struct is implemented
**Then** it contains `entry: OrderParams`, `take_profit: OrderParams`, `stop_loss: OrderParams`
**And** entry is always a market order; take_profit is limit; stop_loss is stop

**Given** the `core` crate
**When** `BracketState` enum is implemented
**Then** it includes variants: `NoBracket`, `EntryOnly`, `EntryAndStop`, `Full`, `Flattening`

**Given** the `core` crate
**When** `FillEvent` struct is implemented
**Then** it contains `order_id: u64`, `fill_price: FixedPrice`, `fill_size: u32`, `timestamp: UnixNanos`, `side: Side`

**Given** the `core` crate
**When** `Position` struct is implemented
**Then** it tracks `symbol_id: u32`, `side: Option<Side>`, `quantity: u32`, `avg_entry_price: FixedPrice`, `unrealized_pnl: FixedPrice`
**And** it provides methods to apply fills and compute P&L in quarter-ticks

**Given** property tests exist for order state transitions
**When** arbitrary state transitions are attempted
**Then** only valid transitions succeed (e.g., `Idle → Submitted`, `Submitted → Confirmed`, `Submitted → Rejected`, `Submitted → Uncertain`)
**And** invalid transitions are rejected (e.g., `Idle → Filled`, `Cancelled → Submitted`)

### Story 1.5: Core Traits & Clock

As a developer,
I want well-defined trait interfaces for signals, regime detection, broker connectivity, and time,
So that implementations can be swapped (live vs test) while maintaining consistent contracts.

**Acceptance Criteria:**

**Given** the `core` crate `traits/signal.rs`
**When** the `Signal` trait is defined
**Then** it requires: `fn update(&mut self, book: &OrderBook, trade: Option<&MarketEvent>, clock: &dyn Clock) -> Option<f64>`
**And** `fn name(&self) -> &'static str`
**And** `fn is_valid(&self) -> bool`
**And** `fn reset(&mut self)` for replay clean-slate
**And** `fn snapshot(&self) -> SignalSnapshot` for determinism verification
**And** `SignalSnapshot` is a struct capturing signal state for comparison

**Given** the `core` crate `traits/regime.rs`
**When** the `RegimeDetector` trait is defined
**Then** it requires: `fn update(&mut self, bar: &Bar, clock: &dyn Clock) -> RegimeState`
**And** `fn current(&self) -> RegimeState`
**And** `RegimeState` enum has variants: `Trending`, `Rotational`, `Volatile`, `Unknown`

**Given** the `core` crate `traits/broker.rs`
**When** the `BrokerAdapter` trait is defined
**Then** it requires async methods for: subscribe to market data, submit order, cancel order, query positions, query open orders
**And** all methods return `Result<T, BrokerError>`
**And** `BrokerError` is defined with variants: `ConnectionLost`, `OrderRejected`, `DeserializationFailed`, `Timeout`, `PositionQueryFailed`

**Given** the `core` crate `traits/clock.rs`
**When** the `Clock` trait is defined
**Then** it requires: `fn now(&self) -> UnixNanos` (monotonic) and `fn wall_clock(&self) -> chrono::DateTime<chrono::Utc>` (display only)
**And** `SystemClock` struct implements `Clock` using `std::time::Instant` for monotonic time and `chrono::Utc::now()` for wall clock
**And** the trait is `Send + Sync`

### Story 1.6: Event System & Config Types

As a developer,
I want a complete event taxonomy and typed configuration structures,
So that every system event has a well-defined type and all configuration is validated at startup.

**Acceptance Criteria:**

**Given** the `core` crate `events/`
**When** the event types are implemented
**Then** `SignalEvent` contains `signal_type: SignalType`, `value: f64`, `timestamp: UnixNanos`, `decision_id: u64`
**And** `SignalType` enum includes `Obi`, `Vpin`, `Microprice`, `Composite`
**And** `CircuitBreakerEvent` contains breaker type, trigger reason, timestamp, and current state
**And** `RegimeTransition` contains `from: RegimeState`, `to: RegimeState`, `timestamp: UnixNanos`
**And** `ConnectionStateChange` contains `from` and `to` connection states, timestamp
**And** `HeartbeatEvent` contains `timestamp: UnixNanos`, `sequence: u64`
**And** `EngineEvent` enum wraps all event types: `Market(MarketEvent)`, `Signal(SignalEvent)`, `Order(OrderEvent)`, `Fill(FillEvent)`, `Regime(RegimeTransition)`, `CircuitBreaker(CircuitBreakerEvent)`, `Connection(ConnectionStateChange)`, `Heartbeat(HeartbeatEvent)`
**And** `OrderEvent` contains `order_id: u64`, `state: OrderState`, `params: OrderParams`, `timestamp: UnixNanos`, `decision_id: Option<u64>`

**Given** the `core` crate `config/`
**When** config types are implemented
**Then** `TradingConfig` contains `max_daily_loss_ticks: i64`, `max_consecutive_losses: u32`, `max_trades_per_day: u32`, `max_position_size: u32`
**And** `FeeConfig` contains `exchange_per_side_ticks: i64`, `commission_per_side_ticks: i64`, `api_per_side_ticks: i64`, `slippage_ticks: i64`, `minimum_edge_multiple: f64`, `schedule_date: chrono::NaiveDate`
**And** `BrokerConfig` contains connection parameters (host, port, system name, app name) — credentials are NOT in this struct
**And** all config types implement `Debug`, `Clone`, `serde::Deserialize`

**Given** `core/src/config/validation.rs`
**When** semantic validation runs at startup
**Then** it rejects `max_daily_loss_ticks <= 0`
**And** it rejects `max_consecutive_losses == 0`
**And** it rejects `minimum_edge_multiple < 1.0`
**And** it rejects fee values that are negative
**And** it warns if `fee_schedule_date` is >30 days old and blocks if >60 days old
**And** validation returns `Result<(), Vec<ConfigValidationError>>` with all errors collected, not just the first

### Story 1.7: Test Kit

As a developer,
I want comprehensive test utilities available to all crates,
So that every test uses consistent, deterministic helpers rather than ad-hoc test setup.

**Acceptance Criteria:**

**Given** the `testkit` crate
**When** `SimClock` is implemented
**Then** it implements the `Clock` trait from `core`
**And** it provides `advance_by(nanos: u64)` to manually advance time
**And** it provides `set_time(nanos: UnixNanos)` for replay scenarios
**And** `now()` returns the current simulated time (monotonically increasing)
**And** it is deterministic — same sequence of advance calls produces same time values

**Given** the `testkit` crate
**When** `OrderBookBuilder` is implemented
**Then** it provides a fluent API: `OrderBookBuilder::new().bid(price, size, count).ask(price, size, count).build()`
**And** prices can be specified as `f64` (converted to `FixedPrice` internally) for test ergonomics
**And** it validates monotonic price ordering on `build()`
**And** it populates `bid_count` and `ask_count` automatically

**Given** the `testkit` crate
**When** `MockBrokerAdapter` is implemented
**Then** it implements the `BrokerAdapter` trait from `core`
**And** it provides configurable behavior: immediate fill, partial fill, rejection, timeout
**And** it records all submitted orders for later assertion
**And** it provides `set_positions(Vec<Position>)` for reconciliation test scenarios

**Given** the `testkit` crate
**When** market data generators are implemented
**Then** `market_gen` provides functions to generate sequences of `MarketEvent` for testing
**And** pre-built scenarios exist for: normal trading, FOMC volatility spike, flash crash, empty book, reconnection gap

**Given** the `testkit` crate
**When** custom assertions are implemented
**Then** `price_eq_epsilon(a: f64, b: f64, epsilon: f64)` asserts floating-point near-equality for signal values
**And** `signal_in_range(value: f64, min: f64, max: f64)` validates signal output bounds

## Epic 2: Market Data Ingestion & Recording

Connect to Rithmic via rithmic-rs, receive live L1/L2 data, reconstruct order book from streaming updates, detect data gaps, record continuously to Parquet, and ingest historical Parquet files. This epic delivers the data foundation that all signal analysis, replay, and trading builds upon.

### Story 2.1: Rithmic Connection & Market Data Subscription

As a trader-operator,
I want the system to connect to Rithmic and receive live market data,
So that I have real-time L1/L2 feeds for configured CME futures contracts.

**Acceptance Criteria:**

**Given** valid Rithmic credentials in environment variables and broker config in TOML
**When** the RithmicAdapter initializes
**Then** it establishes a WebSocket connection to the Rithmic TickerPlant
**And** credentials are loaded via the `secrecy` crate, never logged or stored in plaintext
**And** the connection uses TLS encryption (NFR14)

**Given** a live Rithmic connection
**When** the adapter subscribes to configured symbols (e.g., MES, MNQ)
**Then** it receives real-time L1 (best bid/ask, last trade) and L2 (depth-of-book) updates
**And** incoming protobuf messages are deserialized via `prost` into internal types

**Given** incoming Rithmic messages
**When** the `message_validator` processes each message
**Then** well-formed messages are converted to `MarketEvent` (via `broker/src/messages.rs`)
**And** malformed messages are logged, skipped, and counted in a sliding 60-second window
**And** if >10 malformed messages occur within any 60-second window, a circuit-break signal is emitted

**Given** a connection failure during initialization
**When** the adapter cannot connect
**Then** it returns a typed `BrokerError::ConnectionLost` with diagnostic context
**And** no partial state is left behind

### Story 2.2: Order Book Reconstruction from Live Feed

As a trader-operator,
I want the order book maintained in real-time from streaming data,
So that signal computation always has current market state.

**Acceptance Criteria:**

**Given** the RithmicAdapter is receiving L1/L2 updates
**When** a bid or ask update arrives
**Then** it is written as a `MarketEvent` to the SPSC ring buffer (rtrb, capacity 131072)
**And** the producer (I/O thread) never blocks — if the buffer is full, the drop is logged explicitly

**Given** the engine event loop is running on its dedicated core
**When** it reads `MarketEvent` entries from the SPSC buffer
**Then** it updates the `OrderBook` in-place (no allocation)
**And** bid levels remain sorted descending by price
**And** ask levels remain sorted ascending by price
**And** `bid_count` and `ask_count` reflect the actual number of populated levels

**Given** SPSC buffer occupancy monitoring
**When** occupancy reaches 50%
**Then** a warning is logged with current occupancy metric
**When** occupancy reaches 80%
**Then** trade evaluation is disabled (data quality gate) while market data processing continues to drain the queue
**When** occupancy reaches 95%
**Then** a full circuit break is triggered

**Given** an integration test with recorded Rithmic market data (via testkit)
**When** a sequence of L2 updates is processed
**Then** the resulting `OrderBook` state matches the expected snapshot
**And** `is_tradeable()` returns correct results for the reconstructed book

### Story 2.3: Market Data Gap Detection

As a trader-operator,
I want the system to detect when market data is stale or has gaps,
So that trading decisions are never made on outdated information.

**Acceptance Criteria:**

**Given** the system is receiving live market data during market hours
**When** no tick is received for longer than a configurable threshold (e.g., 3 seconds)
**Then** the system flags data as stale and activates the data quality gate
**And** a structured log event is emitted with the gap duration and last received timestamp

**Given** the system is receiving market data with sequence numbers
**When** a gap in sequence numbers is detected
**Then** the gap is logged with the missing sequence range
**And** the data quality gate is activated until the gap is resolved or sufficient new data arrives

**Given** the data quality gate is active
**When** fresh data resumes and the gate conditions are no longer met
**Then** the data quality gate auto-clears
**And** a log event records the gap duration and resolution

**Given** market hours have ended or the system is outside trading hours
**When** no ticks arrive
**Then** the stale data detector does NOT trigger false positives
**And** the system correctly distinguishes between "no data because market is closed" and "no data because feed is broken"

### Story 2.4: Continuous Market Data Recording to Parquet

As a trader-operator,
I want all market data recorded to Parquet files continuously,
So that I accumulate historical data for research and replay even when not actively trading.

**Acceptance Criteria:**

**Given** the engine is running and receiving market data
**When** market data events are processed
**Then** they are written to Parquet files partitioned as `data/market/{SYMBOL}/{YYYY-MM-DD}.parquet`
**And** recording occurs regardless of whether trading strategies are active
**And** recording occurs regardless of circuit breaker state

**Given** Parquet writing is implemented
**When** data is written
**Then** it runs asynchronously on the Tokio I/O runtime, not on the hot path thread
**And** it uses columnar Parquet format with compression for storage efficiency
**And** each record includes: timestamp (UnixNanos as i64), price (quarter-ticks as i64), size (u32), side, event type, symbol_id

**Given** the system runs across a date boundary (midnight CT)
**When** the date changes
**Then** a new Parquet file is created for the new date
**And** the previous day's file is properly closed/flushed

**Given** the system shuts down (gracefully or ungracefully)
**When** shutdown occurs
**Then** all buffered market data is flushed to Parquet before exit (graceful)
**And** on ungraceful shutdown, at most the last few seconds of data may be lost (acceptable tradeoff for not blocking hot path)

### Story 2.5: Historical Parquet Ingestion

As a trader-operator,
I want to load historical market data from Parquet files,
So that I can feed it through the replay engine for strategy validation.

**Acceptance Criteria:**

**Given** a Parquet file containing historical market data (from Databento or from our own recording)
**When** the data source is initialized with a file path
**Then** it reads the Parquet file and produces a sequential stream of `MarketEvent` structs
**And** events are ordered by timestamp (ascending)
**And** Databento-format Parquet files are correctly mapped to our `MarketEvent` types

**Given** historical data is loaded
**When** it is fed into the event loop
**Then** it uses the same SPSC buffer path as live data (same consumer code)
**And** the data source interface is identical whether the source is live Rithmic or historical Parquet

**Given** multiple Parquet files for a date range
**When** a multi-day replay is requested
**Then** files are loaded in chronological order
**And** events across file boundaries maintain correct timestamp ordering

**Given** a Parquet file with corrupt or missing data
**When** the ingestion encounters bad records
**Then** corrupt records are skipped with a warning log
**And** ingestion continues with the next valid record
**And** a summary of skipped records is reported at the end

## Epic 3: Signal Analysis & Trade Decision Engine

Implement OBI, VPIN, and microprice signals with incremental O(1) updates. Build structural price level identification. Composite signal evaluation with weighted scoring and fee-aware trade gating. All signal computation runs on the hot path with zero allocations.

### Story 3.1: OBI Signal

As a trader-operator,
I want order book imbalance computed in real-time,
So that directional pressure from the order book informs trade decisions.

**Acceptance Criteria:**

**Given** the `engine/src/signals/obi.rs` module
**When** `ObiSignal` is implemented
**Then** it implements the `Signal` trait from `core`
**And** `update()` computes OBI from the current `OrderBook` bid/ask sizes in O(1) — no iteration beyond top N levels
**And** OBI value is in the range [-1.0, 1.0] where positive = bid pressure, negative = ask pressure
**And** `is_valid()` returns `false` until sufficient data has been processed (configurable warmup period)
**And** `reset()` clears all internal state for replay clean-slate
**And** `snapshot()` captures current OBI value and internal state for determinism verification

**Given** an `OrderBook` where `is_tradeable()` returns `false`
**When** `update()` is called
**Then** it returns `None` (invalid book = no signal)

**Given** an empty or zero-size book
**When** `update()` is called
**Then** it returns `None` — never produces NaN or Inf

**Given** unit tests with known order book configurations (via `OrderBookBuilder`)
**When** OBI is computed
**Then** results match expected values within epsilon tolerance (1e-10)
**And** all tests use `SimClock` from testkit

### Story 3.2: VPIN Signal

As a trader-operator,
I want volume-synchronized probability of informed trading computed in real-time,
So that I can detect informed flow activity that precedes price moves.

**Acceptance Criteria:**

**Given** the `engine/src/signals/vpin.rs` module
**When** `VpinSignal` is implemented
**Then** it implements the `Signal` trait from `core`
**And** `update()` processes trade events to classify volume into buy/sell buckets
**And** volume buckets are time-aware via injected `Clock` (not direct time access)
**And** VPIN value is in the range [0.0, 1.0] where higher = more informed trading
**And** `is_valid()` returns `false` until sufficient volume buckets have been filled (configurable)
**And** `reset()` clears all bucket state for replay
**And** `snapshot()` captures bucket state for determinism verification

**Given** no trade events have been received (only book updates)
**When** `update()` is called with `trade: None`
**Then** it returns the last computed VPIN or `None` if insufficient data
**And** no computation is wasted on non-trade updates

**Given** unit tests with known trade sequences
**When** VPIN is computed over the sequence
**Then** results match expected values within epsilon tolerance
**And** bucket boundaries align with configured volume thresholds

### Story 3.3: Microprice Signal

As a trader-operator,
I want a volume-weighted mid-price computed in real-time,
So that I have a more accurate estimate of fair value than simple mid.

**Acceptance Criteria:**

**Given** the `engine/src/signals/microprice.rs` module
**When** `MicropriceSignal` is implemented
**Then** it implements the `Signal` trait from `core`
**And** `update()` computes microprice from top-of-book bid/ask prices and sizes: `microprice = (bid_price * ask_size + ask_price * bid_size) / (bid_size + ask_size)`
**And** the result is returned as `f64` (signal value, not a price for comparison)
**And** `is_valid()` returns `false` when book has zero sizes on either side
**And** `reset()` clears state for replay
**And** `snapshot()` captures state for determinism

**Given** an `OrderBook` with equal bid and ask sizes
**When** microprice is computed
**Then** it equals the simple mid-price

**Given** an `OrderBook` with larger ask size than bid size
**When** microprice is computed
**Then** it is closer to the bid price (market leans toward the side with less size)

**Given** unit tests with `OrderBookBuilder`
**When** various book configurations are tested
**Then** microprice matches hand-calculated expected values within epsilon

### Story 3.4: Structural Price Levels

As a trader-operator,
I want key structural price levels identified automatically,
So that signals are evaluated at meaningful market levels rather than continuously.

**Acceptance Criteria:**

**Given** the `engine/src/signals/` module (structural levels)
**When** the level engine is initialized
**Then** it identifies prior day high and low from historical data (previous session's Parquet or configured values)
**And** it identifies volume point of control (VPOC) — the price with highest volume in the prior session
**And** it supports manually configured key reaction levels (from config file)

**Given** structural levels are computed
**When** the current price approaches a structural level (within configurable proximity threshold in quarter-ticks)
**Then** the level engine flags that price is "at level"
**And** signal evaluation is triggered (signals fire at levels, not continuously)

**Given** a new trading session begins
**When** the level engine refreshes
**Then** prior day H/L updates to the most recent completed session
**And** VPOC recalculates from the most recent session data
**And** the refresh is logged with the new level values

**Given** no historical data is available (first run)
**When** the level engine initializes
**Then** it operates with manually configured levels only
**And** it logs a warning that automatic levels are unavailable

### Story 3.5: Composite Evaluation & Fee-Aware Gating

As a trader-operator,
I want all signals combined into a single trade/no-trade decision with fee-aware filtering,
So that only high-probability, profitable-after-costs trades are taken.

**Acceptance Criteria:**

**Given** `engine/src/signals/mod.rs` with `SignalPipeline`
**When** the pipeline is constructed
**Then** it contains named fields: `obi: ObiSignal`, `vpin: VpinSignal`, `microprice: MicropriceSignal`
**And** it is concrete types with zero vtable overhead (no `dyn Signal`)

**Given** the `engine/src/signals/composite.rs` module
**When** composite evaluation runs
**Then** ALL signals must be valid (`is_valid() == true`) — if any signal is invalid, no trade decision is generated
**And** a weighted composite score is computed from signal values (weights from config)
**And** expected edge is converted to `FixedPrice` via banker's rounding
**And** a unique `decision_id: u64` is generated for every evaluation (for causality tracing)

**Given** the `engine/src/risk/fee_gate.rs` module
**When** `FeeGate` evaluates a potential trade
**Then** total round-trip cost = 2 * (exchange + commission + api) + slippage (all in quarter-ticks)
**And** the trade is permitted only if expected edge > `minimum_edge_multiple` * total round-trip cost
**And** if `fee_schedule_date` is >30 days old, a warning is logged
**And** if `fee_schedule_date` is >60 days old, ALL trade evaluations are blocked (gate active)
**And** the staleness gate never blocks flattening or safety orders — only trade evaluation

**Given** a composite evaluation where all conditions are met
**When** the decision is "trade"
**Then** the output includes: direction (Side), expected edge (FixedPrice), composite score, decision_id, and all individual signal values
**And** the decision and all contributing values are logged with the decision_id for post-hoc analysis

**Given** a composite evaluation where any condition fails
**When** the decision is "no trade"
**Then** the reason is logged (which signal invalid, or edge below threshold, or fee gate active)
**And** the decision_id still tracks the evaluation for analysis

## Epic 4: Order Execution, Position Management & Event Journal

SQLite event journal (WAL mode) as the persistence foundation. Market order submission via Rithmic OrderPlant. Atomic bracket orders with exchange-resting stops. Order state machine with Uncertain/PendingRecon states. WAL persistence before submission. Position reconciliation with broker.

### Story 4.1: SQLite Event Journal

As a trader-operator,
I want all system events persisted to a durable journal,
So that I have a complete audit trail and can recover state after crashes.

**Acceptance Criteria:**

**Given** the `engine/src/persistence/journal.rs` module
**When** the event journal is initialized
**Then** it creates/opens a SQLite database at `data/journal.db` in WAL mode
**And** tables are created if they don't exist: `trade_events`, `order_states`, `circuit_breaker_events`, `regime_transitions`, `system_events`
**And** all timestamps are stored as `INTEGER` (Unix nanoseconds)
**And** all prices are stored as `INTEGER` (quarter-ticks)

**Given** the journal receives events via a bounded crossbeam channel (capacity 8192)
**When** `EngineEvent` variants are written
**Then** each event type is routed to its appropriate table
**And** writes happen asynchronously on the Tokio runtime — never blocking the hot path
**And** every trade-related event includes `decision_id` for causality tracing (NFR17)

**Given** the system crashes
**When** it restarts
**Then** the SQLite WAL ensures no committed data is lost (NFR9)
**And** uncommitted in-flight events (last few ms) may be lost — this is acceptable

**Given** the journal is running
**When** periodic WAL checkpointing occurs
**Then** the WAL file is checkpointed to prevent unbounded growth
**And** checkpointing does not block event writes

### Story 4.2: Market Order Submission

As a trader-operator,
I want the system to submit market orders via the broker,
So that trade decisions can be executed against the exchange.

**Acceptance Criteria:**

**Given** the `broker/src/order_routing.rs` module
**When** an `OrderEvent` is received from the SPSC order queue (rtrb, capacity 4096)
**Then** the order is submitted to the Rithmic OrderPlant via the broker connection
**And** the order includes: symbol, side, quantity, order type (market)

**Given** an order is submitted
**When** the exchange confirms or rejects
**Then** a `FillEvent` (for fills) or state update is written to the fill SPSC queue (rtrb, capacity 4096) back to the engine
**And** fill events include: order_id, fill_price, fill_size, timestamp, side

**Given** the engine receives a fill
**When** it processes the `FillEvent`
**Then** the order state transitions from `Confirmed` to `Filled` (or `PartialFill`)
**And** the fill is written to the event journal with the associated decision_id

**Given** an order is rejected by the exchange
**When** the rejection is received
**Then** the order state transitions to `Rejected`
**And** the rejection reason is logged with full diagnostic context
**And** the rejection is written to the event journal

### Story 4.3: Atomic Bracket Orders

As a trader-operator,
I want every trade protected by exchange-resting stops from the moment of entry,
So that I never have an unprotected position.

**Acceptance Criteria:**

**Given** a trade decision with direction, take-profit level, and stop-loss level
**When** a `BracketOrder` is constructed
**Then** `entry` is a market order
**And** `take_profit` is a limit order at the configured profit target (exchange-resting)
**And** `stop_loss` is a stop order at the configured stop level (exchange-resting)
**And** TP and SL are submitted as OCO (one-cancels-other) at the exchange

**Given** the entry order fills
**When** bracket legs are submitted
**Then** `BracketState` transitions from `EntryOnly` to `EntryAndStop` (when stop confirms) to `Full` (when both confirm)
**And** each transition is tracked per position

**Given** the entry fills but bracket leg submission fails
**When** the failure is detected
**Then** a flatten retry loop begins: submit market flatten order
**And** if rejected, wait 1 second, retry (up to 3 attempts)
**And** after 3 failed attempts, **panic mode** activates: all trading disabled, all entry orders cancelled (stops preserved), operator alerted, system requires manual restart

**Given** a bracket order's take-profit or stop-loss fills
**When** the fill is received
**Then** the OCO counterpart is automatically cancelled by the exchange
**And** the position is updated to flat
**And** P&L is computed in quarter-ticks and logged to the event journal

### Story 4.4: Order State Machine & WAL Persistence

As a trader-operator,
I want every order state transition validated and durably persisted,
So that no order is ever in an unknown state, even after crashes.

**Acceptance Criteria:**

**Given** the `engine/src/order_manager/state_machine.rs` module
**When** an order state transition is attempted
**Then** only valid transitions are permitted (per the defined state machine: Idle→Submitted, Submitted→Confirmed, Submitted→Rejected, Submitted→Uncertain, etc.)
**And** invalid transitions log an error and trigger a circuit breaker
**And** all transitions are exhaustive match arms — no catch-all/wildcard

**Given** the `engine/src/order_manager/wal.rs` module
**When** an order is about to transition to `Submitted`
**Then** the order state is written to SQLite WAL *before* the submission is sent to the broker
**And** on crash recovery, the startup sequence replays uncommitted orders from the journal to determine true state

**Given** an order is in `Submitted` state for >5 seconds without confirmation or rejection
**When** the timeout expires
**Then** the order transitions to `Uncertain`
**And** new order submission is immediately paused
**And** the broker is queried for the order's actual status (`PendingRecon`)

**Given** an `Uncertain` order with a bracket already submitted (stop resting at exchange)
**When** reconciliation discovers the order is filled
**Then** the bracket protects the position — no additional action needed beyond state correction

**Given** an `Uncertain` order without a bracket (unprotected)
**When** reconciliation discovers an open position
**Then** immediate flatten is triggered (unprotected position is the worst state)

### Story 4.5: Position Tracking & Broker Reconciliation

As a trader-operator,
I want local position state always consistent with the broker,
So that I never have phantom positions or missed fills.

**Acceptance Criteria:**

**Given** the `engine/src/order_manager/tracker.rs` module
**When** fills are received
**Then** local `Position` state is updated: quantity, side, average entry price
**And** unrealized P&L is computed using current market price (from OrderBook)
**And** realized P&L is computed on position close (exit price - entry price in quarter-ticks * tick value)

**Given** the position tracker
**When** reconciliation is triggered (on startup, reconnection, or periodic schedule)
**Then** the broker is queried for current positions via `BrokerAdapter::query_positions()`
**And** local state is compared against broker-reported state

**Given** a position mismatch between local and broker state
**When** the mismatch is detected
**Then** a circuit breaker is triggered immediately (NFR16) — no silent correction
**And** the mismatch details are logged: local state, broker state, discrepancy
**And** the system halts trading until manual review

**Given** positions are consistent
**When** reconciliation completes
**Then** a success event is logged with both local and broker state for audit
**And** the system continues normal operation

## Epic 5: Risk Management & Circuit Breakers

Tiered circuit breakers (gates vs breakers) protecting capital at every layer. Position limits, daily loss, consecutive losses, max trades, anomalous positions, buffer overflow, data quality, fee staleness, malformed messages. Panic mode escalation. Event-aware trading windows. Operator alerting.

### Story 5.1: Circuit Breaker Framework

As a trader-operator,
I want a unified risk management framework checked before every order,
So that no trade can bypass safety checks regardless of market conditions.

**Acceptance Criteria:**

**Given** the `engine/src/risk/circuit_breakers.rs` module
**When** the `CircuitBreakers` struct is implemented
**Then** it is the single source of truth for "can we trade?"
**And** it contains state for all gate and breaker types
**And** it distinguishes **gates** (auto-clear when condition resolves) from **breakers** (require manual reset/restart)

**Given** the circuit breaker framework
**When** `permits_trading()` is called
**Then** ALL breakers and gates are checked synchronously
**And** it returns a `Result` with specific denial reasons if any check fails
**And** checking happens within one tick of the event loop (NFR10) — no deferred checks

**Given** a circuit breaker is tripped
**When** it activates
**Then** entry and limit orders are cancelled
**And** stop-loss orders are PRESERVED (never cancel a stop unless flattening with a market order first)
**And** a `CircuitBreakerEvent` is emitted to the event journal

**Given** a gate condition resolves (e.g., data quality restores)
**When** the gate auto-clears
**Then** trading resumes without manual intervention
**And** the clearance is logged

### Story 5.2: Position & Loss Limit Breakers

As a trader-operator,
I want hard limits on position size, daily losses, consecutive losses, and trade count,
So that no single bad day or streak can cause catastrophic loss.

**Acceptance Criteria:**

**Given** `max_position_size` is configured (e.g., 2 contracts)
**When** a trade decision would exceed this limit
**Then** the max position size **gate** blocks the order
**And** it auto-clears when position returns within limits (e.g., after a partial close)

**Given** `max_daily_loss_ticks` is configured (e.g., 800 quarter-ticks = $200 on MES)
**When** cumulative daily realized + unrealized loss exceeds the threshold
**Then** the daily loss **breaker** activates — session is over
**And** it requires manual reset (restart) to resume
**And** the breaker persists across reconnections within the same session

**Given** `max_consecutive_losses` is configured (e.g., 5)
**When** N consecutive losing trades occur
**Then** the consecutive loss **breaker** activates
**And** the counter resets on any winning trade (not on breaker reset)
**And** it requires manual reset to resume

**Given** `max_trades_per_day` is configured (e.g., 20)
**When** the trade count is exceeded
**Then** the max trades **breaker** activates — prevents overtrading
**And** requires manual reset

### Story 5.3: Anomalous Position Detection & Panic Mode

As a trader-operator,
I want the system to detect unexpected positions and escalate to panic mode if flattening fails,
So that I never have unmanaged risk exposure.

**Acceptance Criteria:**

**Given** the system detects a position existing outside any active strategy context
**When** the anomaly is detected
**Then** the anomalous position **breaker** activates immediately
**And** an automated flatten (market order) is submitted

**Given** the `broker/src/position_flatten.rs` module
**When** flatten is attempted
**Then** a market order is submitted to close the position
**And** if rejected: wait 1 second, retry (up to 3 attempts total)
**And** flatten retry returns success/failure to the caller

**Given** the `engine/src/risk/panic_mode.rs` module
**When** all 3 flatten attempts fail
**Then** **panic mode** activates:
**And** all trading is disabled
**And** all entry/limit orders are cancelled (stops preserved)
**And** operator is alerted
**And** the system cannot resume without manual intervention (restart)
**And** panic mode state is logged with full context: position details, flatten attempts, rejection reasons

### Story 5.4: Infrastructure Breakers

As a trader-operator,
I want infrastructure-level safety checks that catch system-level problems,
So that degraded system performance never leads to bad trades.

**Acceptance Criteria:**

**Given** SPSC buffer occupancy monitoring
**When** the MarketEvent buffer reaches 95% capacity
**Then** the buffer overflow **breaker** activates (manual reset required)
**And** this is distinct from the 80% data quality gate (which only pauses trade evaluation)

**Given** market data quality monitoring
**When** data is stale, feed has gaps, or `OrderBook::is_tradeable()` returns false
**Then** the data quality **gate** activates (auto-clears when quality restores)

**Given** the fee schedule configuration
**When** `fee_schedule_date` exceeds 60 days
**Then** the fee staleness **gate** activates — blocks trade evaluation
**And** safety/flattening orders are never gated by fee staleness
**And** auto-clears when config is updated with a fresh schedule date

**Given** the malformed message counter
**When** >10 malformed Rithmic messages occur within a sliding 60-second window
**Then** the malformed message **breaker** activates (manual reset — feed may be corrupt)

**Given** the reconnection FSM
**When** it enters CircuitBreak state (reconciliation mismatch or timeout)
**Then** the connection failure **breaker** activates (manual reset — state may be corrupt)

### Story 5.5: Event-Aware Trading Windows

As a trader-operator,
I want the system to automatically reduce exposure during high-impact scheduled events,
So that I avoid the outsized risk of FOMC, CPI, NFP, and similar announcements.

**Acceptance Criteria:**

**Given** a config section for event-aware trading windows
**When** events are configured (e.g., `[[events]]` with datetime, duration, and action)
**Then** each event specifies: start time, end time (or duration), and action (disable_strategies / reduce_exposure / sit_out)

**Given** the current time falls within a configured event window
**When** the event window check runs
**Then** the configured action is applied: strategies disabled, or position size reduced, or system sits out entirely
**And** the event activation is logged with event name and action taken

**Given** the event window expires
**When** the end time passes
**Then** normal trading parameters resume automatically
**And** the resumption is logged

**Given** the Clock trait is used for time checks
**When** event windows are evaluated
**Then** `clock.wall_clock()` is used (since events are wall-clock scheduled)
**And** in replay mode, SimClock's wall_clock determines which events are active

### Story 5.6: Operator Alerting on Circuit Breaker Events

As a trader-operator,
I want to be notified when circuit breakers activate,
So that I can investigate and intervene when the system halts trading.

**Acceptance Criteria:**

**Given** any circuit breaker (not gate) activates
**When** the activation occurs
**Then** the event is written to a dedicated alert log (separate from general structured logs)
**And** the alert includes: breaker type, trigger reason, timestamp, current position state, current P&L

**Given** panic mode activates
**When** the escalation occurs
**Then** an alert is emitted with severity "critical" including all flatten attempt details

**Given** a simple notification mechanism
**When** an alert is written
**Then** an external notification script/hook is invoked (configurable path in config)
**And** the script receives the alert payload as JSON on stdin
**And** if the script fails or is not configured, the alert is still logged (notification failure never blocks the system)

## Epic 6: Regime Detection & Strategy Orchestration

Threshold-based regime detection classifying market conditions as trending, rotational, volatile, or unknown. Strategy enable/disable based on regime state. Regime transition logging for post-hoc analysis.

### Story 6.1: Threshold-Based Regime Detector

As a trader-operator,
I want the market regime classified in real-time,
So that the system only trades when conditions match the active strategy's edge.

**Acceptance Criteria:**

**Given** the `engine/src/regime/threshold.rs` module
**When** `ThresholdRegimeDetector` is implemented
**Then** it implements the `RegimeDetector` trait from `core`
**And** it processes 1-minute or 5-minute `Bar` data (configurable interval), not every tick
**And** it classifies regime as: `Trending`, `Rotational`, `Volatile`, or `Unknown`
**And** classification uses configurable thresholds on: volatility (ATR-based), directional persistence, and range-to-body ratio

**Given** system startup or insufficient data
**When** fewer bars than the warmup period have been processed
**Then** `current()` returns `RegimeState::Unknown`
**And** `Unknown` regime blocks trading by default (configurable)

**Given** known market data sequences (via testkit scenarios)
**When** the regime detector processes them
**Then** a clear trending sequence (e.g., monotonically increasing bars) classifies as `Trending`
**And** a choppy sideways sequence classifies as `Rotational`
**And** a high-ATR sequence with no direction classifies as `Volatile`
**And** all tests use `SimClock`

### Story 6.2: Strategy Enable/Disable & Regime Transition Logging

As a trader-operator,
I want strategies automatically enabled or disabled based on regime,
So that the system sits out unfavorable conditions without manual intervention.

**Acceptance Criteria:**

**Given** a regime-to-strategy mapping in configuration
**When** the regime state changes
**Then** strategies permitted in the new regime are enabled
**And** strategies not permitted are disabled
**And** disabling a strategy does NOT cancel existing stop-loss orders (positions remain protected)

**Given** a regime transition occurs
**When** the transition is detected
**Then** a `RegimeTransition` event is emitted with: `from`, `to`, `timestamp`
**And** the event is written to the event journal for post-hoc analysis
**And** the transition is included in structured logs

**Given** the regime is `Unknown` at startup
**When** sufficient bars accumulate and a regime is classified
**Then** the first transition from `Unknown` to a known regime is logged
**And** strategy enabling follows the new regime's permitted strategy set

**Given** rapid regime oscillation (flipping between states)
**When** transitions occur faster than a configurable cooldown period
**Then** the system remains in the more conservative state (does not enable trading on brief regime blips)
**And** the oscillation pattern is logged for research analysis

## Epic 7: Historical Replay & Paper Trading

Replay orchestrator wiring SimClock + MockBroker + Parquet data source through the same event loop as live. Deterministic replay verification. Paper trading mode with live data and simulated execution. Paper trade result recording with full fidelity.

### Story 7.1: Replay Orchestrator

As a trader-operator,
I want to replay historical data through the exact same engine code path as live trading,
So that I can validate strategy logic against known data.

**Acceptance Criteria:**

**Given** the `engine/src/replay/mod.rs` module
**When** the engine is started with `--replay <parquet-path>` CLI flag
**Then** it wires: `SimClock` (from testkit) instead of `SystemClock`
**And** `MockBrokerAdapter` (from testkit) instead of `RithmicAdapter`
**And** Parquet data source (from Story 2.5) feeds `MarketEvent` into the same SPSC buffer
**And** the event loop code is identical — no replay-specific branching in the hot path

**Given** replay is running
**When** `SimClock` advances
**Then** time advances based on recorded event timestamps (synthetic monotonic time)
**And** no real-time waiting occurs between events — elapsed time is compressed
**And** targeting ≥100x real-time throughput (NFR5)

**Given** the MockBrokerAdapter in replay mode
**When** orders are submitted
**Then** fills are simulated based on configurable fill model (immediate fill at market price as V1 default)
**And** simulated fills flow back through the same FillEvent SPSC queue

**Given** replay completes
**When** all historical data is processed
**Then** a summary is output: total events, trades taken, P&L, win rate, max drawdown
**And** all events are written to the event journal (same as live)

### Story 7.2: Deterministic Replay Verification

As a trader-operator,
I want identical data to produce identical results every time,
So that I can trust replay as a validation tool for signal and strategy changes.

**Acceptance Criteria:**

**Given** the same Parquet data file and the same configuration
**When** replay is run twice
**Then** all `FixedPrice` values (prices, P&L) are bit-identical between runs
**And** all signal values are identical within epsilon tolerance (1e-10)
**And** `RegimeState` values are identical at every bar boundary

**Given** `Signal::reset()` is called before replay begins
**When** replay runs
**Then** no residual state from prior runs affects results

**Given** `Signal::snapshot()` is called at configurable intervals during replay
**When** snapshots from two identical runs are compared
**Then** they match exactly (per the three-tier determinism model: fixed-point = bitwise, signals = epsilon, regime = snapshot)

**Given** integration tests in `engine/tests/replay_determinism.rs`
**When** a known dataset is replayed
**Then** results are compared against committed snapshot files (via `insta` crate)
**And** any regression in determinism causes test failure

### Story 7.3: Paper Trading Mode

As a trader-operator,
I want to run against live market data without submitting real orders,
So that I can validate the system in real market conditions before risking capital.

**Acceptance Criteria:**

**Given** the engine is started with `--config config/paper.toml`
**When** paper trading mode is active
**Then** live market data flows from Rithmic through the real data path (SPSC, order book, signals)
**And** trade decisions are generated normally (composite evaluation, fee gating, regime checks)
**And** when a trade decision fires, orders are routed to `MockBrokerAdapter` instead of the real Rithmic OrderPlant
**And** `MockBrokerAdapter` simulates fills (immediate fill at current market price as V1 default)

**Given** paper trading is running
**When** the system operates
**Then** the `SystemClock` is used (real time, since market data is live)
**And** circuit breakers, risk limits, and regime detection all function normally
**And** the only difference from live is the order routing destination

**Given** paper mode and live mode
**When** the configuration is compared
**Then** the switch is a single config parameter (broker mode: paper vs live)
**And** no code path differences exist beyond the BrokerAdapter implementation injected at startup

### Story 7.4: Paper Trade Result Recording

As a trader-operator,
I want paper trade results recorded with the same fidelity as live trades,
So that I can analyze paper trading performance with the same tools as live.

**Acceptance Criteria:**

**Given** paper trading is active
**When** simulated fills occur
**Then** they are written to the SQLite event journal with a `source: paper` tag
**And** all fields match live trade recording: order_id, fill_price, fill_size, timestamp, side, decision_id, P&L

**Given** paper trade results in the journal
**When** queried for performance analysis
**Then** P&L, win rate, max drawdown, and trade count are computable from the journal
**And** paper trades are distinguishable from live trades by the source tag

**Given** the validation progression (unit tests → replay → paper → live)
**When** paper trading shows positive expectancy over 500+ trades
**Then** the journal contains sufficient data to make a go/no-go decision for live deployment
**And** all supporting evidence (signal values, regime states, fee calculations) is traceable via decision_id

## Epic 8: System Lifecycle & Reconnection

Startup sequence, graceful shutdown, 5-state reconnection FSM with mandatory reconciliation, full configuration loading, and deployment workflow. The system operates autonomously during market hours and recovers gracefully from failures.

### Story 8.1: Configuration Loading

As a trader-operator,
I want layered configuration with semantic validation,
So that I can safely manage different configurations for paper and live trading.

**Acceptance Criteria:**

**Given** the `engine/src/config/loader.rs` module
**When** configuration is loaded at startup
**Then** it loads in layers: `config/default.toml` → `config/paper.toml` or `config/live.toml` (based on CLI flag) → environment variables
**And** later layers override earlier layers
**And** the `config` crate (0.15.22) handles layering and TOML parsing
**And** environment variables override TOML values (e.g., `TRADING__MAX_DAILY_LOSS_TICKS=800`)

**Given** configuration is loaded
**When** semantic validation runs (from `core/src/config/validation.rs`)
**Then** all config values are validated for semantic correctness (not just parse validity)
**And** all validation errors are collected and reported together (not fail-on-first)
**And** invalid configuration prevents startup with a clear error message

**Given** broker credentials
**When** they are loaded
**Then** they come from environment variables only (via `secrecy` crate)
**And** they are never logged, even at debug level
**And** `.env` file is supported for local development (gitignored)

### Story 8.2: Startup Sequence

As a trader-operator,
I want a safe, deterministic startup sequence,
So that the system never begins trading in an inconsistent state.

**Acceptance Criteria:**

**Given** the `engine/src/lifecycle/startup.rs` module
**When** the system starts
**Then** it executes in strict order:
1. Load and validate configuration
2. Initialize SQLite event journal (WAL mode)
3. Connect to Rithmic (TickerPlant + OrderPlant)
4. Verify connectivity (heartbeat exchange)
5. Replay order WAL — determine true state of any in-flight orders from prior session
6. Sync positions — query broker, compare with local state from journal
7. Arm circuit breakers with configured thresholds
8. Start watchdog heartbeat emission
9. Begin market data ingestion
10. Enable strategies (if regime permits)

**Given** any step in the startup sequence fails
**When** the failure occurs
**Then** the system logs the failure with full context and exits cleanly
**And** no partial startup state is left behind
**And** the exit code indicates which step failed

**Given** the WAL replay discovers in-flight orders from a prior crash
**When** reconciliation determines their true state
**Then** orders confirmed filled update local position state
**And** orders in uncertain state trigger position query and reconciliation
**And** orphaned orders are cancelled

### Story 8.3: Graceful Shutdown

As a trader-operator,
I want a clean shutdown that leaves no orphaned positions or orders,
So that I can safely restart the system without risk exposure.

**Acceptance Criteria:**

**Given** the `engine/src/lifecycle/shutdown.rs` module
**When** a shutdown signal is received (SIGTERM, SIGINT, or programmatic)
**Then** it executes in strict order:
1. Disable all strategies (no new trade decisions)
2. Cancel all open entry/limit orders (preserve stops until positions are flat)
3. Wait for pending order confirmations (with timeout)
4. Verify all positions are flat
5. Cancel remaining stop orders (now safe — no positions)
6. Disconnect from broker
7. Flush event journal (checkpoint WAL)
8. Flush Parquet buffers
9. Stop heartbeat
10. Exit cleanly

**Given** positions exist at shutdown time
**When** the system cannot flatten within the timeout
**Then** it logs a warning with current position state
**And** exchange-resting stops remain in place (they protect the position independently)
**And** the operator is alerted

**Given** a forced shutdown (SIGKILL or crash)
**When** the process terminates unexpectedly
**Then** exchange-resting stops protect any open positions (they live at the exchange, not in our process)
**And** on next startup, the WAL replay and reconciliation recover state

### Story 8.4: Reconnection FSM

As a trader-operator,
I want the system to automatically recover from broker disconnections,
So that brief network blips don't require manual intervention.

**Acceptance Criteria:**

**Given** the `engine/src/connection/fsm.rs` module
**When** the reconnection FSM is implemented
**Then** it has 5 states: `Connected`, `Disconnected`, `Reconnecting`, `Reconciling`, `Ready`
**And** additionally a terminal `CircuitBreak` state for unrecoverable scenarios

**Given** the broker connection drops
**When** a WebSocket disconnection is detected (within 5 seconds per NFR7)
**Then** FSM transitions: `Connected` → `Disconnected`
**And** all strategies are immediately paused (no new orders)
**And** existing exchange-resting stops remain active

**Given** the FSM is in `Disconnecting` state
**When** reconnection is attempted
**Then** FSM transitions to `Reconnecting`
**And** reconnection uses exponential backoff: initial=1s, max=60s, with jitter
**And** each attempt is logged

**Given** the broker connection is re-established
**When** reconnection succeeds
**Then** FSM transitions to `Reconciling`
**And** positions are queried from Rithmic PnL plant
**And** open orders are queried from Rithmic OrderPlant
**And** results are compared against local state (from journal + order manager)
**And** the system waits for consistent state (poll until stable for 5 seconds)

**Given** reconciliation succeeds (local matches broker)
**When** state is consistent
**Then** FSM transitions to `Ready`
**And** strategies are re-armed (if regime permits)

**Given** reconciliation fails (mismatch or timeout after 60 seconds)
**When** the failure is detected
**Then** FSM transitions to `CircuitBreak`
**And** the connection failure breaker activates (manual reset required)
**And** full diagnostic context is logged: local state, broker state, discrepancies

### Story 8.5: Deployment Workflow

As a trader-operator,
I want a safe deployment process with rollback capability,
So that I can update the system without risking data loss or unexpected behavior.

**Acceptance Criteria:**

**Given** a new binary is built
**When** deployment is performed
**Then** the previous binary is kept as `engine.prev` for rollback
**And** the new binary is deployed to `/opt/trading/engine`
**And** the systemd service is restarted: `sudo systemctl restart trading-engine`

**Given** the systemd unit file for `trading-engine`
**When** configured
**Then** it runs the engine binary with the appropriate config flag (--config)
**And** it sets `Restart=on-failure` for automatic restart on crashes
**And** it sets appropriate resource limits
**And** environment variables for credentials are loaded from a systemd environment file

**Given** the deployment safety rule
**When** a deploy is attempted during market hours
**Then** it is only permitted if all positions are flat
**And** the operator must explicitly confirm (deploy script checks position state)

**Given** the binary includes version information
**When** the engine starts
**Then** it logs its version (`env!("CARGO_PKG_VERSION")`) at startup
**And** the version is included in the Prometheus metrics endpoint

## Epic 9: Observability & Monitoring

Structured logging with JSON output and causality tracing. Circuit breaker and regime transition diagnostic logging. Tax reporting data format. Prometheus metrics endpoint. Watchdog heartbeat interface. Credential management.

### Story 9.1: Structured Logging & Causality Tracing

As a trader-operator,
I want structured logs with full causality tracing,
So that I can tail logs in real-time and trace any trade decision back to its contributing signals.

**Acceptance Criteria:**

**Given** the `tracing` crate configured across all crates
**When** logs are emitted
**Then** production output is JSON format (machine-parseable)
**And** development output is human-readable format
**And** every log entry includes: `timestamp`, `level`, `target` (module path), `message`

**Given** trade-related log events
**When** they are emitted
**Then** they include `decision_id` for causality tracing
**And** a full trade decision is traceable: signal values → composite score → fee gate result → order submission → fill → P&L
**And** trading-specific spans include: `order_id`, `signal_type`, `regime_state`

**Given** the hot path event loop
**When** log events are generated
**Then** they are buffered to a channel and written asynchronously
**And** logging never blocks the hot path thread

**Given** the operator tails logs via SSH
**When** `journalctl -u trading-engine -f` is run
**Then** real-time structured output is visible
**And** log output can be filtered by level, module, or decision_id

### Story 9.2: Diagnostic Event Logging

As a trader-operator,
I want detailed diagnostic logs for circuit breaker and regime events,
So that I can understand exactly why the system stopped trading or changed behavior.

**Acceptance Criteria:**

**Given** a circuit breaker activates
**When** the event is logged
**Then** it includes: breaker type, trigger condition, threshold value, actual value, timestamp, current position state, current P&L, all other breaker states

**Given** a regime transition occurs
**When** the event is logged
**Then** it includes: from state, to state, timestamp, contributing indicator values (ATR, directional persistence, etc.)
**And** the log enables post-hoc analysis of regime detection accuracy

**Given** system state changes (connection state, strategy enable/disable)
**When** they are logged
**Then** full diagnostic context is included
**And** state transitions form a coherent timeline in the log stream

### Story 9.3: Tax Reporting Data

As a trader-operator,
I want trade data logged in a format suitable for Section 1256 tax reporting,
So that I can accurately report 60/40 tax treatment for CME futures.

**Acceptance Criteria:**

**Given** the event journal contains trade data
**When** a tax report query is run
**Then** it produces realized P&L per contract, per day
**And** each entry includes: date, symbol, side, quantity, entry price, exit price, realized P&L (in dollars)
**And** the data supports Section 1256 60/40 treatment calculation

**Given** the daily event journal export
**When** `data/events/{YYYY-MM-DD}.parquet` is generated
**Then** trade events are included in a format compatible with tax preparation tools
**And** the Parquet schema matches the journal schema for consistency

### Story 9.4: Prometheus Metrics Endpoint

As a trader-operator,
I want real-time system metrics available via HTTP,
So that I can monitor system health and performance.

**Acceptance Criteria:**

**Given** the `engine/src/metrics.rs` module
**When** the metrics endpoint is initialized
**Then** it exposes a lightweight HTTP endpoint via `axum` on a configurable port
**And** it serves Prometheus-format metrics at `/metrics`

**Given** the metrics endpoint is running
**When** scraped
**Then** it reports:
- `tick_to_decision_latency_ns` (histogram) — hot path processing time
- `spsc_buffer_occupancy` (gauge) — per queue (market, order, fill)
- `order_fill_latency_ns` (histogram) — submission to fill time
- `daily_pnl_ticks` (gauge) — current day's P&L in quarter-ticks
- `connection_state` (gauge) — current FSM state as numeric value
- `circuit_breaker_active` (gauge, per breaker type) — 0 or 1
- `trades_today` (counter) — trade count for current session
- `regime_state` (gauge) — current regime as numeric value
- `engine_version` (info) — from `CARGO_PKG_VERSION`

**Given** metric collection
**When** metrics are updated
**Then** updates happen outside the hot path (via the crossbeam event channel)
**And** the HTTP endpoint runs on the Tokio runtime, not the hot path thread

### Story 9.5: Watchdog Heartbeat Interface

As a trader-operator,
I want the engine to emit heartbeats for an external watchdog,
So that when I deploy the watchdog for live trading, the interface is ready.

**Acceptance Criteria:**

**Given** heartbeat configuration in config
**When** the engine is running
**Then** it emits `HeartbeatEvent` at a configurable interval (default: 10 seconds)
**And** each heartbeat includes a monotonically increasing sequence number and timestamp

**Given** the heartbeat emission mechanism
**When** heartbeats are sent
**Then** they are transmitted via a configurable transport (V1: simple TCP socket or shared file/health endpoint)
**And** the protocol is simple enough that the future watchdog binary can consume it trivially

**Given** the engine is unable to emit a heartbeat (e.g., system overloaded)
**When** a heartbeat is missed
**Then** the miss is logged locally
**And** the next heartbeat includes the gap information

**Given** the watchdog binary does not yet exist (deferred to live-trading preparation)
**When** heartbeats are emitted
**Then** they are also logged to the event journal for testing and verification
**And** the heartbeat interface can be validated in paper trading before the watchdog is built

### Story 9.6: Credential Management

As a trader-operator,
I want broker credentials handled securely,
So that API keys are never exposed in config files, logs, or source control.

**Acceptance Criteria:**

**Given** broker API credentials (username, password, API key)
**When** they are loaded
**Then** they come from environment variables only (NFR11)
**And** they are wrapped in `secrecy::SecretString` which prevents accidental logging/display
**And** `.env` file is supported for local development (loaded at startup, gitignored)
**And** `.env.example` documents all required variables without actual values

**Given** credentials are in memory
**When** any logging occurs
**Then** `SecretString` `Debug` and `Display` implementations emit `[REDACTED]`
**And** credentials never appear in structured log output, error messages, or metrics

**Given** the connection to Rithmic
**When** credentials are used
**Then** they are passed directly to the rithmic-rs connection setup
**And** the connection uses TLS (NFR14)
**And** credentials are zeroized from memory when no longer needed (secrecy crate default behavior)
