# Story 1.5: Core Traits & Clock

Status: review

## Story

As a developer,
I want well-defined trait interfaces for signals, regime detection, broker connectivity, and time,
So that implementations can be swapped (live vs test) while maintaining consistent contracts.

## Acceptance Criteria (BDD)

- Given the `core` crate When the `Signal` trait is defined Then it requires: `fn update(&mut self, book: &OrderBook, trade: Option<&MarketEvent>, clock: &dyn Clock) -> Option<f64>`, `fn name(&self) -> &'static str`, `fn is_valid(&self) -> bool`, `fn reset(&mut self)`, `fn snapshot(&self) -> SignalSnapshot`
- Given the `core` crate When the `RegimeDetector` trait is defined Then it requires: `fn update(&mut self, bar: &Bar, clock: &dyn Clock) -> RegimeState`, `fn current(&self) -> RegimeState`, and `RegimeState` enum has variants: `Trending`, `Rotational`, `Volatile`, `Unknown`
- Given the `core` crate When the `BrokerAdapter` trait is defined Then it requires async methods for: subscribe, submit order, cancel order, query positions, query open orders. All return `Result<T, BrokerError>`.
- Given the `core` crate When the `Clock` trait is defined Then it requires: `fn now(&self) -> UnixNanos` and `fn wall_clock(&self) -> chrono::DateTime<chrono::Utc>`. `SystemClock` implements it. The trait is `Send + Sync`.

## Tasks / Subtasks

### Task 1: Create traits module structure (AC: all traits exist in core)
- 1.1: Create `crates/core/src/traits/mod.rs` with public module declarations for `signal`, `regime`, `broker`, `clock`
- 1.2: Update `crates/core/src/lib.rs` to declare `pub mod traits` and re-export key traits

### Task 2: Implement Clock trait and SystemClock (AC: Clock trait, Send + Sync, SystemClock)
- 2.1: Create `crates/core/src/traits/clock.rs`
- 2.2: Define `pub trait Clock: Send + Sync` with methods:
  - `fn now(&self) -> UnixNanos`
  - `fn wall_clock(&self) -> chrono::DateTime<chrono::Utc>`
- 2.3: Implement `pub struct SystemClock;`
- 2.4: Implement `Clock for SystemClock`:
  - `now()`: use `std::time::SystemTime::now()` converted to nanos since epoch
  - `wall_clock()`: use `chrono::Utc::now()`
- 2.5: Write unit tests for SystemClock: `now()` returns a reasonable timestamp, `wall_clock()` returns current time

### Task 3: Implement Signal trait and SignalSnapshot (AC: Signal trait with all methods)
- 3.1: Create `crates/core/src/traits/signal.rs`
- 3.2: Define `SignalSnapshot` struct with fields: `name: &'static str`, `value: Option<f64>`, `valid: bool`, `timestamp: UnixNanos`
- 3.3: Define `pub trait Signal: Send` with methods:
  - `fn update(&mut self, book: &OrderBook, trade: Option<&MarketEvent>, clock: &dyn Clock) -> Option<f64>`
  - `fn name(&self) -> &'static str`
  - `fn is_valid(&self) -> bool`
  - `fn reset(&mut self)`
  - `fn snapshot(&self) -> SignalSnapshot`
- 3.4: Add doc comments explaining: update is incremental O(1), reset() clears state for replay, snapshot() captures state for deterministic replay

### Task 4: Implement RegimeDetector trait and RegimeState (AC: RegimeDetector trait, RegimeState enum)
- 4.1: Create `crates/core/src/traits/regime.rs`
- 4.2: Define `#[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum RegimeState { Trending, Rotational, Volatile, Unknown }`
- 4.3: Implement `Default for RegimeState` returning `Unknown`
- 4.4: Define `pub trait RegimeDetector: Send` with methods:
  - `fn update(&mut self, bar: &Bar, clock: &dyn Clock) -> RegimeState`
  - `fn current(&self) -> RegimeState`

### Task 5: Implement BrokerAdapter trait and BrokerError (AC: async methods, BrokerError)
- 5.1: Create `crates/core/src/traits/broker.rs`
- 5.2: Define `BrokerError` using thiserror:
  - `ConnectionLost(String)`
  - `OrderRejected { order_id: u64, reason: String }`
  - `DeserializationFailed(String)`
  - `Timeout { operation: String, duration_ms: u64 }`
  - `PositionQueryFailed(String)`
- 5.3: Define `#[async_trait] pub trait BrokerAdapter: Send + Sync` with methods:
  - `async fn subscribe(&mut self, symbol: &str) -> Result<(), BrokerError>`
  - `async fn submit_order(&mut self, params: OrderParams) -> Result<u64, BrokerError>` (returns order_id)
  - `async fn cancel_order(&mut self, order_id: u64) -> Result<(), BrokerError>`
  - `async fn query_positions(&self) -> Result<Vec<Position>, BrokerError>`
  - `async fn query_open_orders(&self) -> Result<Vec<(u64, OrderState)>, BrokerError>`
- 5.4: Note: consider whether to use `async_trait` crate or native async traits (Rust 1.95 supports them natively) — prefer native async fn in trait if dyn dispatch is not needed, or use `async_trait` for dyn compatibility

### Task 6: Write unit tests
- 6.1: Test SystemClock implements Clock trait (compile-time check via usage)
- 6.2: Test BrokerError Display output
- 6.3: Test RegimeState default is Unknown
- 6.4: Verify Clock is Send + Sync (compile-time assertion: `fn assert_send_sync<T: Send + Sync>() {}; assert_send_sync::<SystemClock>();`)

## Dev Notes

### Architecture Patterns & Constraints
- Clock trait is MANDATORY injection everywhere — never call `Instant::now()` or `SystemTime::now()` directly outside of SystemClock
- Signal trait: incremental O(1) update per market event, `reset()` clears all state for replay, `snapshot()` captures state for deterministic audit
- Signal::update returns `Option<f64>` — f64 is permitted for signal values (one of the three allowed f64 uses)
- BrokerAdapter is async because broker communication is inherently I/O-bound
- BrokerError uses thiserror for structured error variants
- All traits should be object-safe where possible (for dyn dispatch in tests)
- Consider whether BrokerAdapter needs `async_trait` or can use native async fn in traits (RPITIT) — Rust 1.95 supports `async fn` in traits but `dyn` dispatch requires `async_trait`

### Project Structure Notes
```
crates/core/src/
└── traits/
    ├── mod.rs
    ├── clock.rs        (Clock trait, SystemClock)
    ├── signal.rs       (Signal trait, SignalSnapshot)
    ├── regime.rs       (RegimeDetector trait, RegimeState)
    └── broker.rs       (BrokerAdapter trait, BrokerError)
```

### References
- Architecture document: `docs/architecture.md` — Section: Core Traits, Clock injection pattern, Signal pipeline
- Epics document: `docs/epics.md` — Epic 1, Story 1.5

## Dev Agent Record

### Agent Model Used
Claude Opus 4.6 (1M context)

### Debug Log References
N/A

### Completion Notes List
- Clock trait (Send+Sync) with SystemClock implementation
- Signal trait with incremental update, reset, snapshot
- RegimeDetector trait with RegimeState enum (Default=Unknown)
- BrokerAdapter async trait with BrokerError (5 variants)
- Added async-trait workspace dependency for dyn-safe async traits
- 5 new unit tests (clock, broker error display, regime default, send+sync)

### Change Log
- 2026-04-16: All tasks completed

### File List
- Cargo.toml (modified - added async-trait)
- crates/core/Cargo.toml (modified - added async-trait)
- crates/core/src/lib.rs (modified)
- crates/core/src/traits/mod.rs (new)
- crates/core/src/traits/clock.rs (new)
- crates/core/src/traits/signal.rs (new)
- crates/core/src/traits/regime.rs (new)
- crates/core/src/traits/broker.rs (new)
