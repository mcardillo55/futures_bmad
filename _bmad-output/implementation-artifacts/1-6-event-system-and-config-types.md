# Story 1.6: Event System & Config Types

Status: ready-for-dev

## Story

As a developer,
I want a complete event taxonomy and typed configuration structures,
So that every system event has a well-defined type and all configuration is validated at startup.

## Acceptance Criteria (BDD)

- Given the `core` crate When the event types are implemented Then `SignalEvent`, `CircuitBreakerEvent`, `RegimeTransition`, `ConnectionStateChange`, `HeartbeatEvent` all exist with specified fields
- And `EngineEvent` enum wraps all: `Market(MarketEvent)`, `Signal(SignalEvent)`, `Order(OrderEvent)`, `Fill(FillEvent)`, `Regime(RegimeTransition)`, `CircuitBreaker(CircuitBreakerEvent)`, `Connection(ConnectionStateChange)`, `Heartbeat(HeartbeatEvent)`
- And `OrderEvent` contains `order_id: u64`, `state: OrderState`, `params: OrderParams`, `timestamp: UnixNanos`, `decision_id: Option<u64>`
- Given config types When implemented Then `TradingConfig`, `FeeConfig`, `BrokerConfig` exist with specified fields
- Given `core/src/config/validation.rs` When semantic validation runs Then it rejects invalid values (negative fees, zero consecutive losses, edge multiple < 1.0, etc.) and warns/blocks on fee schedule staleness

## Tasks / Subtasks

### Task 1: Implement SignalEvent (AC: SignalEvent exists with fields)
- 1.1: Create `crates/core/src/events/signal.rs`
- 1.2: Define `SignalEvent` with fields: `signal_name: &'static str`, `value: f64`, `timestamp: UnixNanos`, `book_snapshot_id: Option<u64>`
- 1.3: Derive `Debug, Clone`

### Task 2: Implement lifecycle events (AC: CircuitBreakerEvent, RegimeTransition, ConnectionStateChange, HeartbeatEvent)
- 2.1: Create `crates/core/src/events/lifecycle.rs`
- 2.2: Define `RegimeTransition` with fields: `from: RegimeState`, `to: RegimeState`, `timestamp: UnixNanos`
- 2.3: Define `ConnectionStateChange` with fields: `connected: bool`, `endpoint: String`, `timestamp: UnixNanos`, `reason: Option<String>`
- 2.4: Define `HeartbeatEvent` with fields: `timestamp: UnixNanos`, `sequence: u64`

### Task 3: Implement risk events (AC: CircuitBreakerEvent)
- 3.1: Create `crates/core/src/events/risk.rs`
- 3.2: Define `CircuitBreakerEvent` with fields: `breaker_type: CircuitBreakerType`, `triggered: bool`, `timestamp: UnixNanos`, `reason: String`
- 3.3: Define `CircuitBreakerType` enum with variants: `DailyLossLimit`, `ConsecutiveLosses`, `MaxDrawdown`, `ConnectionLoss`, `DataGap`

### Task 4: Implement OrderEvent (AC: OrderEvent fields)
- 4.1: Define `OrderEvent` in `crates/core/src/events/order.rs` (or within existing order types) with: `order_id: u64`, `state: OrderState`, `params: OrderParams`, `timestamp: UnixNanos`, `decision_id: Option<u64>`
- 4.2: Derive `Debug, Clone`

### Task 5: Implement EngineEvent enum (AC: wraps all event types)
- 5.1: Create `crates/core/src/events/engine.rs` or add to `events/mod.rs`
- 5.2: Define `pub enum EngineEvent`:
  - `Market(MarketEvent)`
  - `Signal(SignalEvent)`
  - `Order(OrderEvent)`
  - `Fill(FillEvent)`
  - `Regime(RegimeTransition)`
  - `CircuitBreaker(CircuitBreakerEvent)`
  - `Connection(ConnectionStateChange)`
  - `Heartbeat(HeartbeatEvent)`
- 5.3: Implement `EngineEvent::timestamp(&self) -> UnixNanos` that returns the timestamp from whichever variant
- 5.4: Update `crates/core/src/events/mod.rs` to re-export all event types

### Task 6: Create config module structure (AC: config types exist)
- 6.1: Create `crates/core/src/config/mod.rs`
- 6.2: Update `crates/core/src/lib.rs` to declare `pub mod config`

### Task 7: Implement TradingConfig (AC: TradingConfig with fields)
- 7.1: Create `crates/core/src/config/trading.rs`
- 7.2: Define `TradingConfig` with fields: `symbol: String`, `max_position_size: u32`, `max_daily_loss: FixedPrice`, `max_consecutive_losses: u32`, `edge_multiple_threshold: f64`, `session_start: String`, `session_end: String`, `max_spread_threshold: FixedPrice`
- 7.3: Derive `Debug, Clone, serde::Deserialize`

### Task 8: Implement FeeConfig (AC: FeeConfig with fields)
- 8.1: Create `crates/core/src/config/fees.rs`
- 8.2: Define `FeeConfig` with fields: `exchange_fee: f64`, `clearing_fee: f64`, `nfa_fee: f64`, `broker_commission: f64`, `effective_date: String`, `total_per_side(&self) -> f64`
- 8.3: Derive `Debug, Clone, serde::Deserialize`

### Task 9: Implement BrokerConfig (AC: BrokerConfig with fields)
- 9.1: Create `crates/core/src/config/broker.rs`
- 9.2: Define `BrokerConfig` with fields: `server: String`, `gateway: String`, `user: String`, `password: secrecy::SecretString`, `reconnect_delay_ms: u64`, `heartbeat_interval_ms: u64`, `order_timeout_ms: u64`
- 9.3: Derive `Debug, Clone, serde::Deserialize` (with custom Debug that redacts password)

### Task 10: Implement config validation (AC: semantic validation, staleness checks)
- 10.1: Create `crates/core/src/config/validation.rs`
- 10.2: Define `ConfigValidationError` enum with descriptive variants
- 10.3: Implement `validate_trading_config(config: &TradingConfig) -> Result<(), Vec<ConfigValidationError>>`:
  - Reject `max_position_size == 0`
  - Reject `max_consecutive_losses == 0`
  - Reject `edge_multiple_threshold < 1.0`
  - Reject negative max_daily_loss
- 10.4: Implement `validate_fee_config(config: &FeeConfig) -> Result<(), Vec<ConfigValidationError>>`:
  - Reject any negative fee
  - Warn if `effective_date` is >30 days old
  - Block if `effective_date` is >60 days old
- 10.5: Implement `validate_broker_config(config: &BrokerConfig) -> Result<(), Vec<ConfigValidationError>>`:
  - Reject empty server/gateway/user
  - Reject zero timeout values
- 10.6: Implement `validate_all(trading: &TradingConfig, fees: &FeeConfig, broker: &BrokerConfig) -> Result<(), Vec<ConfigValidationError>>` that collects ALL errors (does not short-circuit)

### Task 11: Write unit tests for config validation
- 11.1: Test valid configs pass validation
- 11.2: Test each rejection case individually (negative fees, zero losses, etc.)
- 11.3: Test fee staleness: 29 days passes, 31 days warns, 61 days blocks
- 11.4: Test that validation collects ALL errors, not just the first one

### Task 12: Write unit tests for EngineEvent
- 12.1: Test exhaustive match on EngineEvent (compile-time guarantee)
- 12.2: Test timestamp() returns correct value for each variant

## Dev Notes

### Architecture Patterns & Constraints
- EngineEvent with exhaustive `match` ensures no event type is silently ignored — any new variant causes compile errors at all match sites
- All config types implement `Debug, Clone, serde::Deserialize` for loading from TOML
- BrokerConfig password uses `secrecy::SecretString` — never log or display it
- Config validation returns `Result<(), Vec<ConfigValidationError>>` collecting ALL errors, not short-circuiting on the first one
- Fee staleness: >30 days from `effective_date` produces a warning-level validation error, >60 days produces a blocking error
- MarketEvent and FillEvent already exist from Stories 1.3 and 1.4 — this story adds the remaining event types and the EngineEvent wrapper
- SignalEvent uses f64 for signal value (permitted use of f64)

### Project Structure Notes
```
crates/core/src/
├── events/
│   ├── mod.rs
│   ├── market.rs       (from Story 1.3)
│   ├── signal.rs       (NEW)
│   ├── order.rs        (NEW)
│   ├── lifecycle.rs    (NEW - RegimeTransition, ConnectionStateChange, HeartbeatEvent)
│   ├── risk.rs         (NEW - CircuitBreakerEvent)
│   └── engine.rs       (NEW - EngineEvent wrapper enum)
└── config/
    ├── mod.rs
    ├── trading.rs
    ├── fees.rs
    ├── broker.rs
    └── validation.rs
```

### References
- Architecture document: `docs/architecture.md` — Section: Event System, Configuration Management, Circuit Breakers
- Epics document: `docs/epics.md` — Epic 1, Story 1.6

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
