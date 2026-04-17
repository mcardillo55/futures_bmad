# Story 3.1: OBI Signal

Status: ready-for-dev

## Story

As a trader-operator,
I want order book imbalance computed in real-time,
So that directional pressure from the order book informs trade decisions.

## Acceptance Criteria (BDD)

- Given `engine/src/signals/obi.rs` When `ObiSignal` implemented Then implements Signal trait from `core/src/traits/signal.rs`, `update()` computes OBI from OrderBook bid/ask sizes in O(1) (no iteration beyond top N levels), OBI value in [-1.0, 1.0] where positive = bid pressure / negative = ask pressure, `is_valid()` returns false until sufficient data processed (configurable warmup period), `reset()` clears all internal state for replay clean-slate, `snapshot()` captures current OBI value and internal state for determinism verification
- Given an OrderBook where `is_tradeable()` returns false When `update()` called Then returns None (invalid book = no signal)
- Given an empty or zero-size book When `update()` called Then returns None — never produces NaN or Inf
- Given unit tests with known order book configurations (via OrderBookBuilder) When OBI computed Then results match expected values within epsilon tolerance (1e-10), all tests use SimClock from testkit

## Tasks / Subtasks

### Task 1: Create signals module structure in engine crate (AC: module exists)
- 1.1: Create `crates/engine/src/signals/mod.rs` with `pub mod obi;` declaration
- 1.2: Update `crates/engine/src/lib.rs` to declare `pub mod signals`

### Task 2: Implement ObiSignal struct (AC: Signal trait, O(1) update, value range)
- 2.1: Create `crates/engine/src/signals/obi.rs` with `ObiSignal` struct containing fields: `value: Option<f64>`, `update_count: u64`, `warmup_period: u64` (configurable, e.g. 1 = valid after first update)
- 2.2: Implement `Signal` trait for `ObiSignal`:
  - `update(&mut self, book: &OrderBook, trade: Option<&MarketEvent>, clock: &dyn Clock) -> Option<f64>`:
    - Pre-condition: return `None` if `!book.is_tradeable()`
    - Pre-condition: return `None` if `book.bid_count == 0 || book.ask_count == 0`
    - Pre-condition: return `None` if total size (bid + ask) is zero
    - Compute OBI: `(total_bid_size - total_ask_size) as f64 / (total_bid_size + total_ask_size) as f64`
    - Use top N levels (up to `bid_count` / `ask_count`) — O(1) since N is bounded by fixed array size 10
    - Post-condition: assert result is in [-1.0, 1.0], return `None` if NaN or Inf (defensive)
    - Store value, increment `update_count`
  - `name(&self) -> &'static str`: return `"obi"`
  - `is_valid(&self) -> bool`: return `self.update_count >= self.warmup_period && self.value.is_some()`
  - `reset(&mut self)`: clear `value` to `None`, reset `update_count` to 0
  - `snapshot(&self) -> SignalSnapshot`: capture `value`, `update_count`, name
- 2.3: Implement `ObiSignal::new(warmup_period: u64) -> Self` constructor

### Task 3: NaN/Inf guard implementation (AC: never NaN or Inf)
- 3.1: In `update()`, after computing OBI, check `result.is_finite()` — if false, set `self.value = None` and return `None`
- 3.2: Guard division: if denominator (total_bid_size + total_ask_size) is zero, return `None` before division

### Task 4: Write unit tests (AC: epsilon correctness, SimClock, OrderBookBuilder)
- 4.1: Create `crates/engine/tests/obi_tests.rs` (or inline `#[cfg(test)]` module)
- 4.2: Test: balanced book (equal bid/ask sizes) produces OBI = 0.0 within epsilon
- 4.3: Test: all-bid book (no ask size) — should return None due to is_tradeable or zero denominator guard
- 4.4: Test: heavy bid pressure produces positive OBI close to 1.0
- 4.5: Test: heavy ask pressure produces negative OBI close to -1.0
- 4.6: Test: is_valid() returns false before warmup, true after warmup
- 4.7: Test: reset() clears state, is_valid() returns false again
- 4.8: Test: snapshot() captures current state correctly
- 4.9: Test: update with non-tradeable book returns None
- 4.10: Test: empty book (bid_count=0, ask_count=0) returns None
- 4.11: All tests use `testkit::SimClock::new()` and `testkit::OrderBookBuilder`

## Dev Notes

### Architecture Patterns & Constraints
- OBI formula: `(total_bid_size - total_ask_size) / (total_bid_size + total_ask_size)`
- O(1) computation: OrderBook uses fixed array `[Level; 10]` — iterating all levels is bounded constant time
- Signal values are `f64` (not FixedPrice) — signals output floating point, only final edge converts to FixedPrice
- MANDATORY: `&dyn Clock` parameter in `update()` even if OBI does not use time — the Signal trait requires it
- MANDATORY: pre-condition check `book.is_tradeable()` before any computation
- MANDATORY: NaN/Inf guard — never return non-finite values
- Zero heap allocation in update path
- Concrete type (not `Box<dyn Signal>`) — used as named field in `SignalPipeline`

### Project Structure Notes
```
crates/engine/src/
├── signals/
│   ├── mod.rs          # pub mod obi; (will grow with subsequent stories)
│   └── obi.rs          # ObiSignal implementation

crates/core/src/
├── traits/
│   └── signal.rs       # Signal trait definition (already exists or from prior epic)
├── order_book/
│   └── order_book.rs   # OrderBook struct, Level, is_tradeable()

crates/testkit/src/
├── sim_clock.rs        # SimClock for deterministic tests
└── book_builder.rs     # OrderBookBuilder fluent API
```

### References
- Architecture: Signal Trait definition, OrderBook struct, Clock abstraction
- Epics: Epic 3, Story 3.1
- Dependencies: `core` (Signal trait, OrderBook, Clock, MarketEvent, UnixNanos), `testkit` (SimClock, OrderBookBuilder — dev-dependency)
- Related: Story 3.5 (composite evaluation consumes ObiSignal)

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
