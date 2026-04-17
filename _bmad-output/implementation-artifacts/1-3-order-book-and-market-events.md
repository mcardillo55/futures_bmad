# Story 1.3: Order Book & Market Events

Status: done

## Story

As a developer,
I want a zero-allocation order book and market event types,
So that market data can be processed on the hot path without heap allocations.

## Acceptance Criteria (BDD)

- Given the `core` crate When `Level` struct is implemented Then it contains `price: FixedPrice`, `size: u32`, `order_count: u16` and implements `Debug`, `Clone`, `Copy`
- Given the `core` crate When `OrderBook` struct is implemented Then it contains `bids: [Level; 10]`, `asks: [Level; 10]`, `bid_count: u8`, `ask_count: u8`, `timestamp: UnixNanos`
- And it is stack-allocated with no heap usage (fixed-size arrays)
- And it provides `update_bid(index, level)` and `update_ask(index, level)` for in-place updates
- And it provides `best_bid() -> Option<&Level>` and `best_ask() -> Option<&Level>`
- And it provides `mid_price() -> Option<FixedPrice>` and `spread() -> Option<FixedPrice>`
- And it provides `empty() -> OrderBook` constructor
- Given `OrderBook::is_tradeable()` is implemented When called Then it returns `true` only when: `bid_count >= 3` AND `ask_count >= 3` AND spread is within `max_spread_threshold` AND top-level sizes are non-zero AND bids are descending AND asks are ascending. The `max_spread_threshold` is passed as a parameter.
- Given the `core` crate When `MarketEvent` is implemented Then it contains `timestamp: UnixNanos`, `symbol_id: u32`, `event_type: MarketEventType`, `price: FixedPrice`, `size: u32`, `side: Option<Side>` and implements `Debug`, `Clone`, `Copy`
- Given property tests exist for `OrderBook` When `proptest` generates random order books Then `is_tradeable()` returns `false` for books with fewer than 3 bid or ask levels, `false` for non-monotonic prices, and `best_bid()` always returns highest bid price

## Tasks / Subtasks

### Task 1: Create module structure (AC: types exist in core)
- [x] 1.1: Create `crates/core/src/order_book/mod.rs` with public module declaration
- [x] 1.2: Create `crates/core/src/events/mod.rs` with public module declaration for `market`
- [x] 1.3: Update `crates/core/src/lib.rs` to declare `pub mod order_book` and `pub mod events`

### Task 2: Implement Level struct (AC: Level fields and derives)
- [x] 2.1-2.3: Level struct with price, size, order_count, empty(), Default

### Task 3: Implement OrderBook struct (AC: fixed arrays, stack-allocated, all methods)
- [x] 3.1-3.8: OrderBook with [Level; 10] arrays, update_bid/ask, best_bid/ask, mid_price, spread

### Task 4: Implement is_tradeable (AC: validation logic with max_spread_threshold parameter)
- [x] 4.1-4.6: is_tradeable with bid/ask count, spread, size, monotonicity checks

### Task 5: Implement MarketEvent and MarketEventType (AC: MarketEvent fields and derives)
- [x] 5.1-5.4: MarketEvent, MarketEventType with Trade/BidUpdate/AskUpdate/BookSnapshot

### Task 6: Write property tests (AC: proptest for OrderBook invariants)
- [x] 6.1-6.5: Property tests for tradeable conditions, monotonicity, spread non-negative

### Task 7: Write unit tests for OrderBook methods
- [x] 7.1-7.5: Unit tests for empty, update, mid_price, is_tradeable, out-of-bounds

### Review Findings
- [x] [Review][Patch] Crossed book passes is_tradeable — when best_bid > best_ask (crossed book), spread() returns negative FixedPrice and the spread threshold check does not reject it. Add `if spread.raw() < 0 { return false; }` before the threshold comparison. [crates/core/src/order_book/book.rs:93] — fixed in review patch
- [x] [Review][Decision] update_bid/update_ask gap semantics — calling update_bid(5, level) without populating 0-4 sets bid_count=6, leaving zeroed levels in between. Callers must populate levels contiguously from index 0. Decide whether to (a) document this contract explicitly, (b) enforce contiguity by rejecting index > current count, or (c) accept current behavior as-is since is_tradeable catches invalid books. — fixed in review patch

## Dev Notes

### Architecture Patterns & Constraints
- OrderBook is stack-allocated with fixed-size arrays — zero heap allocation on the hot path
- Book is updated in-place on each market data event, not rebuilt
- `is_tradeable()` is the gatekeeper: must return true before any signal evaluation proceeds
- Level ordering: bids descending (highest first), asks ascending (lowest first)
- Mid price calculation must use integer arithmetic (FixedPrice saturating ops), not float
- `mid_price` with quarter-ticks: `(bid + ask) / 2` may lose half a quarter-tick — this is acceptable
- All types must be `Copy` for zero-cost passing on the hot path

### Project Structure Notes
```
crates/core/src/
├── order_book/
│   ├── mod.rs
│   └── order_book.rs
└── events/
    ├── mod.rs
    └── market.rs

crates/core/tests/
└── order_book_properties.rs
```

### References
- Architecture document: `docs/architecture.md` — Section: OrderBook design, Market Data Pipeline
- Epics document: `docs/epics.md` — Epic 1, Story 1.3

## Dev Agent Record

### Agent Model Used
Claude Opus 4.6 (1M context)

### Debug Log References
Fixed clippy module_inception warning by renaming order_book.rs to book.rs

### Completion Notes List
- OrderBook: stack-allocated [Level; 10] with is_tradeable() gatekeeper
- MarketEvent/MarketEventType: Copy types for hot-path market data
- 9 unit tests + 5 property tests for OrderBook
- Zero clippy warnings, all tests passing

### Change Log
- 2026-04-16: All tasks completed

### File List
- crates/core/src/lib.rs (modified)
- crates/core/src/order_book/mod.rs (new)
- crates/core/src/order_book/book.rs (new)
- crates/core/src/events/mod.rs (new)
- crates/core/src/events/market.rs (new)
- crates/core/tests/order_book_properties.rs (new)
