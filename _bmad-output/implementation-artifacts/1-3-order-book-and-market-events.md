# Story 1.3: Order Book & Market Events

Status: ready-for-dev

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
- 1.1: Create `crates/core/src/order_book/mod.rs` with public module declaration for `order_book`
- 1.2: Create `crates/core/src/events/mod.rs` with public module declaration for `market`
- 1.3: Update `crates/core/src/lib.rs` to declare `pub mod order_book` and `pub mod events`

### Task 2: Implement Level struct (AC: Level fields and derives)
- 2.1: In `crates/core/src/order_book/order_book.rs`, define `#[derive(Debug, Clone, Copy)] pub struct Level { pub price: FixedPrice, pub size: u32, pub order_count: u16 }`
- 2.2: Implement `Level::empty() -> Level` returning zeroed fields
- 2.3: Implement `Default` for `Level`

### Task 3: Implement OrderBook struct (AC: fixed arrays, stack-allocated, all methods)
- 3.1: Define `OrderBook` with `bids: [Level; 10]`, `asks: [Level; 10]`, `bid_count: u8`, `ask_count: u8`, `timestamp: UnixNanos`
- 3.2: Implement `OrderBook::empty() -> OrderBook` with all-zero initialization
- 3.3: Implement `update_bid(index: usize, level: Level)` — updates bid at index, adjusts `bid_count` if needed. Bounds-check index < 10.
- 3.4: Implement `update_ask(index: usize, level: Level)` — updates ask at index, adjusts `ask_count` if needed. Bounds-check index < 10.
- 3.5: Implement `best_bid() -> Option<&Level>` — returns `bids[0]` if `bid_count > 0`
- 3.6: Implement `best_ask() -> Option<&Level>` — returns `asks[0]` if `ask_count > 0`
- 3.7: Implement `mid_price() -> Option<FixedPrice>` — `(best_bid.price + best_ask.price) / 2` using saturating arithmetic, returns None if either side empty
- 3.8: Implement `spread() -> Option<FixedPrice>` — `best_ask.price - best_bid.price`, returns None if either side empty

### Task 4: Implement is_tradeable (AC: validation logic with max_spread_threshold parameter)
- 4.1: Implement `is_tradeable(&self, max_spread_threshold: FixedPrice) -> bool`
- 4.2: Check `bid_count >= 3` and `ask_count >= 3`
- 4.3: Check spread <= max_spread_threshold (using spread() method)
- 4.4: Check top-level bid and ask sizes are non-zero (`bids[0].size > 0 && asks[0].size > 0`)
- 4.5: Check bids are in descending price order (for all populated levels)
- 4.6: Check asks are in ascending price order (for all populated levels)

### Task 5: Implement MarketEvent and MarketEventType (AC: MarketEvent fields and derives)
- 5.1: Create `crates/core/src/events/market.rs`
- 5.2: Define `MarketEventType` enum with variants: `Trade`, `BidUpdate`, `AskUpdate`, `BookSnapshot` (or as appropriate for CME data)
- 5.3: Define `MarketEvent` struct with `timestamp: UnixNanos`, `symbol_id: u32`, `event_type: MarketEventType`, `price: FixedPrice`, `size: u32`, `side: Option<Side>`
- 5.4: Derive `Debug, Clone, Copy` on both types

### Task 6: Write property tests (AC: proptest for OrderBook invariants)
- 6.1: Create `crates/core/tests/order_book_properties.rs`
- 6.2: Property test: `is_tradeable()` returns false when `bid_count < 3` or `ask_count < 3`
- 6.3: Property test: `is_tradeable()` returns false for non-monotonic bid/ask prices
- 6.4: Property test: `best_bid()` always returns the highest bid price (index 0 when properly sorted)
- 6.5: Property test: `spread()` is always non-negative for valid books (asks > bids)

### Task 7: Write unit tests for OrderBook methods
- 7.1: Test `empty()` returns book with zero counts
- 7.2: Test `update_bid` / `update_ask` correctly set levels and counts
- 7.3: Test `mid_price()` calculation for known values
- 7.4: Test `is_tradeable()` with valid and invalid books
- 7.5: Test edge case: index out of bounds on update (should not panic)

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
### Debug Log References
### Completion Notes List
### File List
