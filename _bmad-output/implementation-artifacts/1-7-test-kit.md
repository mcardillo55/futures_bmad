# Story 1.7: Test Kit

Status: ready-for-dev

## Story

As a developer,
I want comprehensive test utilities available to all crates,
So that every test uses consistent, deterministic helpers rather than ad-hoc test setup.

## Acceptance Criteria (BDD)

- Given the `testkit` crate When `SimClock` is implemented Then it implements `Clock` trait, provides `advance_by(nanos: u64)` and `set_time(nanos: UnixNanos)`, is deterministic
- Given the `testkit` crate When `OrderBookBuilder` is implemented Then it provides fluent API with f64 prices, validates monotonic ordering, auto-populates counts
- Given the `testkit` crate When `MockBrokerAdapter` is implemented Then it implements `BrokerAdapter`, provides configurable behavior (fill, partial, reject, timeout), records all submitted orders
- Given the `testkit` crate When market data generators are implemented Then pre-built scenarios exist for: normal trading, FOMC volatility spike, flash crash, empty book, reconnection gap
- Given the `testkit` crate When custom assertions are implemented Then `price_eq_epsilon` and `signal_in_range` exist

## Tasks / Subtasks

### Task 1: Implement SimClock (AC: Clock trait, advance_by, set_time, deterministic)
- 1.1: Create `crates/testkit/src/sim_clock.rs`
- 1.2: Define `pub struct SimClock { current: AtomicU64 }` (or `Cell<u64>` / `RefCell<u64>` — choose based on Send+Sync requirement of Clock trait)
- 1.3: Implement `SimClock::new(start_nanos: u64) -> Self`
- 1.4: Implement `SimClock::advance_by(&self, nanos: u64)` — adds nanos to current time
- 1.5: Implement `SimClock::set_time(&self, time: UnixNanos)` — sets absolute time
- 1.6: Implement `Clock for SimClock`:
  - `now()` returns stored time as UnixNanos
  - `wall_clock()` converts stored time to chrono::DateTime<Utc>
- 1.7: Write tests: advancing time is deterministic, set_time overrides, Clock trait methods work

### Task 2: Implement OrderBookBuilder (AC: fluent API, f64 prices, monotonic validation)
- 2.1: Create `crates/testkit/src/book_builder.rs`
- 2.2: Define `pub struct OrderBookBuilder` with internal bid/ask vectors
- 2.3: Implement fluent methods:
  - `OrderBookBuilder::new() -> Self`
  - `.bid(price: f64, size: u32) -> Self` — converts f64 to FixedPrice, auto-sets order_count to 1
  - `.bid_with_count(price: f64, size: u32, count: u16) -> Self`
  - `.ask(price: f64, size: u32) -> Self`
  - `.ask_with_count(price: f64, size: u32, count: u16) -> Self`
  - `.timestamp(nanos: u64) -> Self`
  - `.build() -> OrderBook` — validates monotonic ordering (bids descending, asks ascending), panics on invalid input in tests
- 2.4: Auto-populate `bid_count` and `ask_count` from number of levels added
- 2.5: Write tests: basic build, validation catches non-monotonic prices, max 10 levels enforced

### Task 3: Implement MockBrokerAdapter (AC: BrokerAdapter impl, configurable behavior, order recording)
- 3.1: Create `crates/testkit/src/mock_broker.rs`
- 3.2: Define `pub enum MockBehavior { Fill, PartialFill(u32), Reject(String), Timeout }`
- 3.3: Define `pub struct MockBrokerAdapter` with:
  - `behavior: MockBehavior` — configurable response behavior
  - `submitted_orders: Vec<OrderParams>` — records all submitted orders
  - `next_order_id: u64` — auto-incrementing order IDs
  - `subscriptions: Vec<String>` — recorded subscriptions
- 3.4: Implement `BrokerAdapter for MockBrokerAdapter`:
  - `subscribe()` — records subscription, returns Ok
  - `submit_order()` — records order, returns based on configured behavior
  - `cancel_order()` — records cancellation
  - `query_positions()` — returns configurable position list
  - `query_open_orders()` — returns configurable order list
- 3.5: Implement inspection methods: `submitted_orders()`, `was_subscribed(symbol)`, `order_count()`
- 3.6: Implement `MockBrokerAdapter::new(behavior: MockBehavior) -> Self`
- 3.7: Write tests: fill behavior, reject behavior, order recording

### Task 4: Implement market data generators (AC: pre-built scenarios)
- 4.1: Create `crates/testkit/src/market_gen.rs`
- 4.2: Implement `pub fn normal_trading_book() -> OrderBook` — tight spread, 5+ levels each side, realistic ES prices around 4500
- 4.3: Implement `pub fn fomc_volatility_spike() -> Vec<OrderBook>` — sequence showing spread widening, thin levels, rapid price movement
- 4.4: Implement `pub fn flash_crash_sequence() -> Vec<OrderBook>` — sequence showing bid side collapse, wide spread, recovery
- 4.5: Implement `pub fn empty_book() -> OrderBook` — zero levels, not tradeable
- 4.6: Implement `pub fn reconnection_gap() -> Vec<OrderBook>` — sequence with time gap simulating connection loss and reconnection

### Task 5: Implement scenario module (AC: organized scenarios)
- 5.1: Create `crates/testkit/src/scenario.rs`
- 5.2: Define `pub struct Scenario { pub name: &'static str, pub books: Vec<OrderBook>, pub events: Vec<MarketEvent> }`
- 5.3: Provide factory methods that combine books + events for each scenario

### Task 6: Implement custom assertions (AC: price_eq_epsilon, signal_in_range)
- 6.1: Create `crates/testkit/src/assertions.rs`
- 6.2: Implement `pub fn price_eq_epsilon(actual: FixedPrice, expected: FixedPrice, epsilon_ticks: i64) -> bool` — checks prices are within epsilon quarter-ticks
- 6.3: Implement `pub fn assert_price_eq_epsilon(actual: FixedPrice, expected: FixedPrice, epsilon_ticks: i64)` — panics with descriptive message on failure
- 6.4: Implement `pub fn signal_in_range(value: f64, min: f64, max: f64) -> bool`
- 6.5: Implement `pub fn assert_signal_in_range(value: f64, min: f64, max: f64)` — panics with descriptive message on failure
- 6.6: Write tests for both assertion functions

### Task 7: Wire up testkit lib.rs (AC: all modules accessible)
- 7.1: Update `crates/testkit/src/lib.rs` to declare all public modules: `sim_clock`, `book_builder`, `mock_broker`, `market_gen`, `scenario`, `assertions`
- 7.2: Add convenience re-exports for commonly used types: `SimClock`, `OrderBookBuilder`, `MockBrokerAdapter`

## Dev Notes

### Architecture Patterns & Constraints
- testkit depends ONLY on core — not on broker or engine. This is critical for the dependency graph.
- SimClock must satisfy `Clock: Send + Sync` — use `AtomicU64` for interior mutability with thread safety, or `std::sync::Mutex` if atomic ops are insufficient
- SimClock is used in ALL tests across all crates — never use SystemClock in tests
- OrderBookBuilder uses f64 prices for ergonomics (test code), converted via `FixedPrice::from_f64` internally
- MockBrokerAdapter implements the `BrokerAdapter` trait from core — this validates that the trait is implementable and object-safe
- Market data generators provide realistic ES futures data: prices around 4400-4600, tick size 0.25, typical spread 0.25-0.50
- All test helpers should have clear, descriptive names and panic with useful messages on misuse

### Project Structure Notes
```
crates/testkit/src/
├── lib.rs
├── sim_clock.rs
├── book_builder.rs
├── mock_broker.rs
├── market_gen.rs
├── scenario.rs
└── assertions.rs
```

### References
- Architecture document: `docs/architecture.md` — Section: Testing Infrastructure, SimClock, Test Utilities
- Epics document: `docs/epics.md` — Epic 1, Story 1.7

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
