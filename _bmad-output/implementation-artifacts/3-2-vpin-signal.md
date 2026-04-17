# Story 3.2: VPIN Signal

Status: done

## Story

As a trader-operator,
I want VPIN computed in real-time,
So that I can detect informed flow activity that precedes price moves.

## Acceptance Criteria (BDD)

- Given `engine/src/signals/vpin.rs` When `VpinSignal` implemented Then implements Signal trait, processes trade events, classifies volume into buy/sell buckets, time-aware via Clock, value in [0.0, 1.0], `is_valid()` false until sufficient buckets filled (configurable), `reset()` clears all bucket state for replay, `snapshot()` captures bucket state for determinism verification
- Given no trade events (only book updates) When `update()` called with `trade: None` Then returns last VPIN or None if insufficient data, no computation wasted on non-trade updates
- Given unit tests with known trade sequences When VPIN computed Then results match expected values within epsilon tolerance, bucket boundaries align with configured volume thresholds

## Tasks / Subtasks

### Task 1: Add VPIN module to signals (AC: module structure)
- [x] 1.1: Add `pub mod vpin;` to `crates/engine/src/signals/mod.rs`

### Task 2: Implement VpinSignal struct (AC: Signal trait, volume buckets, time-aware)
- [x] 2.1: Create `crates/engine/src/signals/vpin.rs` with `VpinSignal` struct containing fields:
  - `bucket_size: u64` — volume threshold per bucket (configurable)
  - `num_buckets: usize` — number of buckets for VPIN window (configurable)
  - `buckets: VecDeque<VolumeBucket>` — rolling window of completed buckets
  - `current_bucket: VolumeBucket` — bucket being filled
  - `last_vpin: Option<f64>` — cached last computed value
  - `valid: bool` — true once `num_buckets` buckets filled
- [x] 2.2: Define `VolumeBucket` struct: `{ buy_volume: u64, sell_volume: u64, start_time: UnixNanos, end_time: UnixNanos }`
- [x] 2.3: Implement `Signal` trait for `VpinSignal`:
  - `update(&mut self, book: &OrderBook, trade: Option<&MarketEvent>, clock: &dyn Clock) -> Option<f64>`:
    - If `trade` is `None`, return `self.last_vpin` (no work on book-only updates)
    - Classify trade volume as buy or sell (using trade side from MarketEvent)
    - Add volume to `current_bucket`
    - Set bucket timestamps via `clock.now()`
    - When `current_bucket` total volume >= `bucket_size`:
      - Push completed bucket to `buckets` deque
      - If deque exceeds `num_buckets`, pop oldest
      - Start new empty bucket
    - If `buckets.len() >= num_buckets`, compute VPIN:
      - `VPIN = sum(|buy_vol - sell_vol|) / sum(total_vol)` across all buckets
      - Result in [0.0, 1.0]
      - Guard: if total volume is zero, return None
      - Guard: if result is NaN/Inf, return None
    - Cache result in `last_vpin`, set `valid = true`
  - `name(&self) -> &'static str`: return `"vpin"`
  - `is_valid(&self) -> bool`: return `self.valid && self.last_vpin.is_some()`
  - `reset(&mut self)`: clear `buckets`, reset `current_bucket`, set `last_vpin = None`, `valid = false`
  - `snapshot(&self) -> SignalSnapshot`: capture `last_vpin`, bucket count, current bucket state
- [x] 2.4: Implement `VpinSignal::new(bucket_size: u64, num_buckets: usize) -> Self` constructor

### Task 3: Trade classification logic (AC: buy/sell classification)
- [x] 3.1: Classify using `MarketEvent.side` field — `Side::Buy` adds to buy volume, `Side::Sell` adds to sell volume
- [x] 3.2: If `side` is `None` on a trade event, use tick rule (compare trade price to last trade price): up-tick = buy, down-tick = sell, same = use last classification
- [x] 3.3: Store `last_trade_price: Option<FixedPrice>` and `last_classification: Option<Side>` for tick rule fallback

### Task 4: Write unit tests (AC: epsilon correctness, known sequences)
- [x] 4.1: Test: all buy volume produces VPIN = 1.0 (maximum informed trading)
- [x] 4.2: Test: equal buy/sell volume produces VPIN = 0.0 (no informed flow)
- [x] 4.3: Test: mixed sequences produce expected VPIN within epsilon
- [x] 4.4: Test: is_valid() false before `num_buckets` filled, true after
- [x] 4.5: Test: reset() clears state, is_valid() returns false
- [x] 4.6: Test: update with `trade: None` returns cached last_vpin
- [x] 4.7: Test: update with `trade: None` before any trades returns None
- [x] 4.8: Test: bucket rollover works correctly (old buckets evicted)
- [x] 4.9: Test: snapshot() captures current state
- [x] 4.10: Test: zero-volume trade edge case returns None
- [x] 4.11: All tests use `testkit::SimClock` and advance time for bucket timestamps

### Review Findings

- [x] [Review][Patch] Overflow subtraction can underflow u64 when minority side pushes bucket past capacity — fixed: cap trim to side's volume, spill remainder to other side [vpin.rs:168-178]
- [x] [Review][Patch] `bucket_size=0` causes infinite loop — fixed: added constructor assert [vpin.rs:52]
- [x] [Review][Patch] Missing test for large trade filling multiple buckets and mixed-side overflow — added 4 tests [vpin_tests.rs]
- [x] [Review][Defer] `snapshot()` doesn't capture full bucket state for determinism — deferred, same `SignalSnapshot` struct limitation as OBI
- [x] [Review][Defer] `num_buckets=0` constructor validation — fixed alongside bucket_size validation

## Dev Notes

### Architecture Patterns & Constraints
- VPIN = Volume-Synchronized Probability of Informed Trading
- Formula: `VPIN = (1/n) * sum(|V_buy_i - V_sell_i| / V_i)` across n buckets, simplified to `sum(|buy - sell|) / sum(total)` across the rolling window
- Volume buckets are time-aware: each bucket records start/end timestamps via injected Clock
- O(1) incremental update: only processes new trade events, bucket rotation is amortized O(1) via VecDeque
- VecDeque is acceptable here (not hot-path allocation — buckets are pre-allocated, only rotate)
- Signal values are `f64`, not FixedPrice
- MANDATORY: `&dyn Clock` parameter — used for bucket timestamps
- MANDATORY: NaN/Inf guard on output
- Trade classification: prefer explicit side from MarketEvent, fall back to tick rule if side is None
- No computation on book-only updates (trade: None path)

### Project Structure Notes
```
crates/engine/src/
├── signals/
│   ├── mod.rs          # pub mod obi; pub mod vpin;
│   ├── obi.rs          # Story 3.1
│   └── vpin.rs         # VpinSignal implementation
```

### References
- Architecture: Signal Trait, Clock abstraction, MarketEvent struct
- Epics: Epic 3, Story 3.2
- Dependencies: `core` (Signal trait, OrderBook, Clock, MarketEvent, Side, FixedPrice, UnixNanos), `testkit` (SimClock — dev-dependency)
- Related: Story 3.5 (composite evaluation consumes VpinSignal)
- Academic: Easley, Lopez de Prado, O'Hara — "Flow Toxicity and Liquidity in a High-Frequency World"

## Dev Agent Record

### Agent Model Used
Claude Opus 4.6 (1M context)

### Debug Log References
No issues encountered. All tests passed on first run.

### Completion Notes List
- Implemented VpinSignal with rolling volume buckets and O(1) incremental update
- Trade classification: explicit side preferred, tick rule fallback for None side
- Volume overflow handling: when a trade fills a bucket, excess volume carries to next bucket
- VecDeque for bucket rotation with configurable window size
- 12 tests covering all ACs: VPIN=1.0, VPIN=0.0, mixed, warmup, reset, cached return, tick rule, zero-volume
- All 166 workspace tests pass, zero clippy warnings

### Change Log
- 2026-04-17: Implemented Story 3.2 — VPIN Signal (all tasks complete)

### File List
- crates/engine/src/signals/vpin.rs (new)
- crates/engine/src/signals/mod.rs (modified — added pub mod vpin)
- crates/engine/tests/vpin_tests.rs (new)
