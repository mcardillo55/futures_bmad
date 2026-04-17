# Story 2.2: Order Book Reconstruction from Live Feed

Status: ready-for-dev

## Story

As a trader-operator,
I want the order book maintained in real-time from streaming data,
So that signal computation always has current market state.

## Acceptance Criteria (BDD)

- Given RithmicAdapter receiving L1/L2 When bid/ask update arrives Then written as MarketEvent to SPSC ring buffer (rtrb, capacity 131072), producer never blocks, full buffer = log drop
- Given engine event loop on dedicated core When it reads MarketEvent from SPSC Then updates OrderBook in-place (no allocation), bids descending, asks ascending, counts accurate
- Given SPSC buffer monitoring When 50% filled then warning logged; 80% filled then trade evaluation disabled; 95% filled then full circuit break
- Given integration test with recorded data When L2 updates processed Then OrderBook matches expected snapshot, is_tradeable() correct

## Tasks / Subtasks

### Task 1: Create SPSC ring buffer bridge (AC: SPSC, producer never blocks, drop on full)
- 1.1: Create `engine/src/spsc.rs` with `MarketEventQueue` wrapper around `rtrb::RingBuffer<MarketEvent>` with capacity 131072
- 1.2: Implement producer side (`MarketEventProducer`) with non-blocking `try_push()` — on full buffer, log drop at `warn` level and increment drop counter
- 1.3: Implement consumer side (`MarketEventConsumer`) with `try_pop()` returning `Option<MarketEvent>`
- 1.4: Implement `fill_fraction()` method using `rtrb::RingBuffer::slots()` arithmetic for threshold monitoring

### Task 2: Implement buffer threshold monitoring (AC: 50%/80%/95% thresholds)
- 2.1: Create `engine/src/buffer_monitor.rs` with `BufferMonitor` struct tracking current fill level
- 2.2: Implement threshold checks: 50% -> log warning, 80% -> return `BufferState::TradingDisabled`, 95% -> return `BufferState::CircuitBreak`
- 2.3: Implement hysteresis to avoid flapping — threshold activates on crossing up, deactivates when dropping below threshold minus 5%
- 2.4: Integrate monitor into event loop so state transitions are acted on each iteration

### Task 3: Implement OrderBook (AC: in-place update, correct ordering)
- 3.1: Create `engine/src/order_book.rs` with `OrderBook` struct using fixed-size arrays or pre-allocated `Vec` for bid/ask levels (no dynamic allocation after init)
- 3.2: Implement `apply_l1_update(&mut self, event: &MarketEvent)` updating best bid/ask in-place
- 3.3: Implement `apply_l2_update(&mut self, event: &MarketEvent)` updating depth levels — bids sorted descending by price, asks sorted ascending by price
- 3.4: Implement `is_tradeable(&self) -> bool` returning true when both bid and ask are present and spread is non-negative
- 3.5: Implement `best_bid()`, `best_ask()`, `spread()`, `mid_price()` accessor methods
- 3.6: Ensure all mutations are in-place — no `Vec::push`, no `Box::new`, no allocator calls on the hot path

### Task 4: Implement engine event loop consumer side (AC: dedicated core, in-place update)
- 4.1: Create `engine/src/event_loop.rs` with `EventLoop` struct holding `MarketEventConsumer`, `OrderBook`, and `BufferMonitor`
- 4.2: Implement `run()` loop: pop event from SPSC, check buffer thresholds, apply event to OrderBook
- 4.3: Pin event loop thread to dedicated core using `core_affinity` crate
- 4.4: Event loop must never allocate — all state is pre-initialized

### Task 5: Wire producer side to broker adapter (AC: SPSC bridge)
- 5.1: In `broker/src/market_data.rs` or a new `engine/src/ingest.rs`, create async task that reads from `MarketDataStream` and pushes to `MarketEventProducer`
- 5.2: Ensure I/O thread (Tokio) produces, hot-path thread consumes — no shared mutexes

### Task 6: Integration test with recorded data (AC: snapshot matching)
- 6.1: Create test fixture with recorded L2 update sequence (hardcoded or from small test file)
- 6.2: Feed updates through SPSC -> OrderBook pipeline
- 6.3: Assert OrderBook state matches expected snapshot at each step (bid/ask levels, sizes, ordering)
- 6.4: Assert `is_tradeable()` returns correct values for various book states (empty, one-sided, crossed, valid)

### Task 7: Unit tests (AC: all)
- 7.1: Test SPSC producer drops events on full buffer and increments counter
- 7.2: Test SPSC consumer returns None on empty buffer
- 7.3: Test OrderBook bid descending / ask ascending invariant after random updates
- 7.4: Test buffer monitor threshold transitions and hysteresis
- 7.5: Test OrderBook `is_tradeable()` edge cases

## Dev Notes

### Architecture Patterns & Constraints
- The SPSC queue is the boundary between the I/O world (Tokio, async) and the hot path (pinned thread, sync). No other synchronization crosses this boundary.
- `MarketEvent` must be `Copy` or at minimum `Clone` + small enough for efficient ring buffer transfer — keep it a flat struct with no heap pointers
- OrderBook uses pre-allocated storage. Typical CME L2 depth is 10 levels per side — allocate for 20 levels per side as headroom
- Buffer capacity 131072 (2^17) is chosen for power-of-2 alignment with rtrb internals
- The event loop thread should use `core_affinity::set_for_current(CoreId)` at thread start before entering the loop

### Project Structure Notes
```
crates/engine/
├── src/
│   ├── lib.rs
│   ├── spsc.rs              (MarketEventQueue, Producer, Consumer)
│   ├── buffer_monitor.rs    (threshold monitoring)
│   ├── order_book.rs        (OrderBook, in-place updates)
│   └── event_loop.rs        (hot-path consumer loop)
```

### References
- Architecture document: `docs/architecture.md` — Section: Event Loop, SPSC Design, OrderBook
- Epics document: `docs/epics.md` — Epic 2, Story 2.2
- Dependencies: rtrb 0.3.3, core_affinity 0.8.3, tracing 0.1.44

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
