# Story 2.3: Market Data Gap Detection

Status: done

## Story

As a trader-operator,
I want the system to detect when market data is stale or has gaps,
So that trading decisions are never made on outdated information.

## Acceptance Criteria (BDD)

- Given live data during market hours When no tick received for configurable threshold (e.g. 3s) Then flags data stale, activates data quality gate, logs gap duration
- Given sequence numbers When gap detected Then logged with missing range, data quality gate activated
- Given data quality gate active When fresh data resumes Then gate auto-clears, gap duration logged
- Given outside market hours When no ticks arrive Then stale detector does NOT trigger false positives

## Tasks / Subtasks

### Task 1: Define Clock trait and implementations (AC: market hours awareness, testability)
- [x] 1.1: Create `core/src/clock.rs` with `Clock` trait providing `now() -> Timestamp`, `is_market_open() -> bool`
- [x] 1.2: Implement `SystemClock` in core for production use, using chrono for CME market hours schedule (Sun 5pm CT - Fri 4pm CT, with daily maintenance break 4pm-5pm CT)
- [x] 1.3: Implement `SimClock` in testkit for deterministic test control with `set_time()`, `advance()`, `set_market_open(bool)` methods

### Task 2: Implement stale data detector (AC: configurable threshold, market hours)
- [x] 2.1: Create `engine/src/data_quality.rs` with `StaleDataDetector` struct holding last_tick_time, threshold duration, and Clock reference
- [x] 2.2: Implement `on_tick(&mut self, timestamp: Timestamp)` updating last_tick_time
- [x] 2.3: Implement `check_stale(&self) -> Option<StaleDuration>` that returns Some with gap duration when elapsed > threshold AND market is open, returns None when market is closed or data is fresh
- [x] 2.4: Make threshold configurable via TOML config (default 3 seconds)

### Task 3: Implement sequence gap detector (AC: sequence numbers, missing range)
- [x] 3.1: Create `SequenceGapDetector` in `engine/src/data_quality.rs` tracking last seen sequence number per symbol
- [x] 3.2: Implement `check_sequence(&mut self, symbol_id: u32, seq: u64) -> Option<GapRange>` returning missing range (expected, received) on gap
- [x] 3.3: Log detected gaps at `warn` level with symbol, expected sequence, received sequence

### Task 4: Implement data quality gate (AC: gate activation, auto-clear)
- [x] 4.1: Create `DataQualityGate` in `engine/src/data_quality.rs` with state: `Open` (data good) or `Gated` (data suspect)
- [x] 4.2: Implement `activate(&mut self, reason: GateReason)` transitioning to Gated state, logging reason and timestamp
- [x] 4.3: Implement `clear(&mut self)` transitioning to Open state, logging gap duration (time between activate and clear)
- [x] 4.4: Gate is a **gate** not a breaker — it auto-clears when fresh data arrives, no manual reset required
- [x] 4.5: Implement `is_open(&self) -> bool` for event loop to check before allowing trade evaluation

### Task 5: Integrate into event loop (AC: all)
- [x] 5.1: Add `StaleDataDetector`, `SequenceGapDetector`, and `DataQualityGate` to `EventLoop` struct
- [x] 5.2: On each MarketEvent: call `on_tick()`, call `check_sequence()`, if either detects issue then activate gate
- [x] 5.3: On each MarketEvent when gate is active: if data is now fresh and sequence is contiguous, auto-clear gate
- [x] 5.4: When gate is active, skip trade signal evaluation (but continue updating OrderBook and recording data)

### Task 6: Periodic stale check (AC: stale detection during silence)
- [x] 6.1: In event loop, implement periodic stale check (e.g. every 500ms via timer or every N loop iterations) since stale detection requires checking even when no events arrive
- [x] 6.2: If stale detected during periodic check, activate gate

### Task 7: Unit tests (AC: all)
- [x] 7.1: Test stale detector triggers after threshold elapsed with SimClock
- [x] 7.2: Test stale detector does NOT trigger outside market hours with SimClock
- [x] 7.3: Test sequence gap detection identifies missing range correctly
- [x] 7.4: Test sequence gap detection passes on contiguous sequences
- [x] 7.5: Test gate auto-clears when fresh data arrives and logs gap duration
- [x] 7.6: Test gate does not flap — remains gated until data is actually fresh
- [x] 7.7: Test integration: stale + sequence gap both activate gate, fresh data clears it

## Dev Notes

### Architecture Patterns & Constraints
- Data quality gate is distinct from circuit breaker — gate auto-clears on recovery, circuit breaker requires manual reset or specific conditions
- The gate affects trade evaluation only — OrderBook updates and Parquet recording continue regardless of gate state
- Clock trait enables deterministic testing — all time-dependent logic uses `Clock` not `std::time::Instant` directly
- CME market hours: Sunday 5:00 PM CT to Friday 4:00 PM CT, with daily maintenance break 4:00 PM - 5:00 PM CT (Mon-Thu)
- Stale detection during silence requires a timer-based check since the event loop only runs on incoming events — consider a secondary timer channel or polling approach

### Project Structure Notes
```
crates/engine/
├── src/
│   ├── data_quality.rs     (StaleDataDetector, SequenceGapDetector, DataQualityGate)
│   └── event_loop.rs       (integration point)
crates/core/
├── src/
│   └── clock.rs            (Clock trait, SystemClock)
crates/testkit/
├── src/
│   └── clock.rs            (SimClock)
```

### References
- Architecture document: `docs/architecture.md` — Section: Data Quality, Risk Gates
- Epics document: `docs/epics.md` — Epic 2, Story 2.3
- Dependencies: chrono 0.4.44, tracing 0.1.44

## Dev Agent Record

### Agent Model Used
Claude Opus 4.6 (1M context)

### Debug Log References
N/A

### Completion Notes List
- Added is_market_open() to Clock trait with CME market hours implementation (is_cme_market_open)
- Added set_market_open() to SimClock for deterministic test control
- StaleDataDetector: configurable threshold, on_tick() + check_stale() with market hours awareness
- SequenceGapDetector: per-symbol sequence tracking with gap range detection, warn logging
- DataQualityGate: Open/Gated state machine with auto-clear on fresh data, gap duration logging
- Integrated all three into EventLoop<C: Clock> with periodic stale check every 1000 iterations
- EventLoop now generic over Clock for testability
- 14 new data quality tests + 4 event loop integration tests, 130 total workspace tests pass

### File List
- crates/core/src/traits/clock.rs (modified — added is_market_open, is_cme_market_open)
- crates/testkit/src/sim_clock.rs (modified — added market_open field, set_market_open)
- crates/engine/Cargo.toml (modified — added testkit dev-dependency)
- crates/engine/src/lib.rs (modified — added data_quality module)
- crates/engine/src/data_quality.rs (new — StaleDataDetector, SequenceGapDetector, DataQualityGate)
- crates/engine/src/event_loop.rs (modified — integrated data quality, generic over Clock)
- crates/engine/tests/order_book_integration.rs (modified — updated for EventLoop<Clock> API)

### Change Log
- 2026-04-17: Implemented Story 2.3 — Market Data Gap Detection (all 7 tasks)
- 2026-04-17: Code review fixes — DST-aware market hours, wired sequence gap detection into event loop, fixed gate auto-clear race, added sequence field to MarketEvent
