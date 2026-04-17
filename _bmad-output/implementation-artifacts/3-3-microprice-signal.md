# Story 3.3: Microprice Signal

Status: done

## Story

As a trader-operator,
I want a volume-weighted mid-price computed in real-time,
So that I have a more accurate estimate of fair value than simple mid.

## Acceptance Criteria (BDD)

- Given `engine/src/signals/microprice.rs` When `MicropriceSignal` implemented Then implements Signal trait, computes `microprice = (bid_price * ask_size + ask_price * bid_size) / (bid_size + ask_size)`, result as f64, `is_valid()` false when zero sizes on either side, `reset()` and `snapshot()` work correctly
- Given equal bid/ask sizes When computed Then equals simple mid-price
- Given larger ask size than bid size When computed Then closer to bid price (market leans toward side with less size)
- Given unit tests with OrderBookBuilder When various configs tested Then microprice matches hand-calculated values within epsilon

## Tasks / Subtasks

### Task 1: Add microprice module to signals (AC: module structure)
- [x] 1.1: Add `pub mod microprice;` to `crates/engine/src/signals/mod.rs`

### Task 2: Implement MicropriceSignal struct (AC: Signal trait, formula, f64 output)
- [x] 2.1: Create `crates/engine/src/signals/microprice.rs` with `MicropriceSignal` struct containing fields:
  - `value: Option<f64>` — last computed microprice
  - `valid: bool` — true when last computation succeeded
- [x] 2.2: Implement `Signal` trait for `MicropriceSignal`:
  - `update(&mut self, book: &OrderBook, trade: Option<&MarketEvent>, clock: &dyn Clock) -> Option<f64>`:
    - Pre-condition: return `None` if `!book.is_tradeable()`
    - Pre-condition: return `None` if `book.bid_count == 0 || book.ask_count == 0`
    - Extract top-of-book: `bid_price = book.bids[0].price`, `bid_size = book.bids[0].size`, `ask_price = book.asks[0].price`, `ask_size = book.asks[0].size`
    - Pre-condition: return `None` if `bid_size == 0 || ask_size == 0` (zero sizes)
    - Pre-condition: return `None` if `bid_size + ask_size == 0` (redundant but defensive)
    - Compute: `microprice = (bid_price.to_f64() * ask_size as f64 + ask_price.to_f64() * bid_size as f64) / (bid_size + ask_size) as f64`
    - Post-condition: if result is NaN or Inf, set `self.valid = false`, return `None`
    - Store in `self.value`, set `self.valid = true`
    - Return `Some(microprice)`
  - `name(&self) -> &'static str`: return `"microprice"`
  - `is_valid(&self) -> bool`: return `self.valid && self.value.is_some()`
  - `reset(&mut self)`: set `value = None`, `valid = false`
  - `snapshot(&self) -> SignalSnapshot`: capture `value`, `valid` state
- [x] 2.3: Implement `MicropriceSignal::new() -> Self` constructor (no configuration needed)

### Task 3: Write unit tests (AC: epsilon correctness, hand-calculated values, OrderBookBuilder)
- [x] 3.1: Test: equal sizes (bid_size=100, ask_size=100) — microprice equals simple mid `(bid + ask) / 2` within epsilon
- [x] 3.2: Test: larger ask size (bid_size=100, ask_size=300) — microprice closer to bid price
  - Example: bid=4482.00 (17928), ask=4482.25 (17929), bid_size=100, ask_size=300
  - microprice = (4482.00 * 300 + 4482.25 * 100) / 400 = 4482.0625
  - Verify result within epsilon of 4482.0625
- [x] 3.3: Test: larger bid size (bid_size=300, ask_size=100) — microprice closer to ask price
  - microprice = (4482.00 * 100 + 4482.25 * 300) / 400 = 4482.1875
- [x] 3.4: Test: extreme imbalance (bid_size=1, ask_size=1000) — microprice very close to bid price
- [x] 3.5: Test: is_valid() false initially, true after successful update
- [x] 3.6: Test: is_valid() false when book has zero sizes on either side
- [x] 3.7: Test: reset() clears state, is_valid() returns false
- [x] 3.8: Test: snapshot() captures current state
- [x] 3.9: Test: non-tradeable book returns None
- [x] 3.10: Test: empty book returns None
- [x] 3.11: All tests use `testkit::SimClock` and `testkit::OrderBookBuilder`

## Dev Notes

### Architecture Patterns & Constraints
- Microprice formula: `(bid_price * ask_size + ask_price * bid_size) / (bid_size + ask_size)`
- Intuition: when ask size >> bid size, microprice moves toward bid (large ask = selling pressure, fair value closer to bid)
- Output is `f64` signal value, NOT FixedPrice — signal values use floating point
- Uses `FixedPrice::to_f64()` for price conversion in formula (display-only method repurposed for signal computation — acceptable per architecture: "f64 permitted for signal output values")
- Zero-allocation computation: reads directly from OrderBook fixed arrays
- O(1): only uses top-of-book (index 0) from bids and asks arrays
- MANDATORY: `&dyn Clock` parameter in `update()` signature even though microprice does not use time — required by Signal trait
- MANDATORY: NaN/Inf guard — never return non-finite values
- MANDATORY: pre-condition check `book.is_tradeable()` before computation

### Project Structure Notes
```
crates/engine/src/
├── signals/
│   ├── mod.rs          # pub mod obi; pub mod vpin; pub mod microprice;
│   ├── obi.rs          # Story 3.1
│   ├── vpin.rs         # Story 3.2
│   └── microprice.rs   # MicropriceSignal implementation
```

### References
- Architecture: Signal Trait, OrderBook struct (Level.price, Level.size), FixedPrice::to_f64()
- Epics: Epic 3, Story 3.3
- Dependencies: `core` (Signal trait, OrderBook, Clock, MarketEvent, FixedPrice), `testkit` (SimClock, OrderBookBuilder — dev-dependency)
- Related: Story 3.5 (composite evaluation consumes MicropriceSignal)

## Dev Agent Record

### Agent Model Used
Claude Opus 4.6 (1M context)

### Debug Log References
No issues encountered. All tests passed on first run.

### Completion Notes List
- Implemented MicropriceSignal with volume-weighted mid-price formula using top-of-book
- O(1) computation, zero allocation, NaN/Inf guards, is_tradeable gate
- Constructor takes max_spread (same pattern as OBI) for is_tradeable validation
- Clock timestamp stored for snapshot determinism (review learning from 3.1)
- Early-return paths clear value/valid (review learning from 3.1)
- 11 tests covering all ACs: equal sizes, bid/ask imbalance, extreme imbalance, warmup, reset, snapshot, edge cases
- All 181 workspace tests pass, zero clippy warnings

### Change Log
- 2026-04-17: Implemented Story 3.3 — Microprice Signal (all tasks complete)

### File List
- crates/engine/src/signals/microprice.rs (new)
- crates/engine/src/signals/mod.rs (modified — added pub mod microprice)
- crates/engine/tests/microprice_tests.rs (new)
