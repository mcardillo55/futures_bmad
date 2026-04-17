# Story 5.4: Infrastructure Breakers

Status: ready-for-dev

## Story

As a trader-operator,
I want infrastructure-level safety checks that catch system-level problems,
So that degraded system performance never leads to bad trades.

## Acceptance Criteria (BDD)

- Given SPSC buffer at 95% capacity When detected Then buffer overflow breaker activates (manual reset), distinct from 80% data quality gate
- Given data quality issues (stale, gaps, `!is_tradeable()`) When detected Then data quality gate activates (auto-clears when quality restores)
- Given `fee_schedule_date` >60 days When detected Then fee staleness gate activates (blocks trade eval), safety/flatten never gated, auto-clears on config update
- Given >10 malformed Rithmic messages in 60s sliding window When detected Then malformed message breaker activates (manual reset)
- Given reconnection FSM enters CircuitBreak When detected Then connection failure breaker activates (manual reset)

## Tasks / Subtasks

### Task 1: Implement buffer overflow breaker (AC: 95% threshold, manual reset, distinct from 80%)
- 1.1: In CircuitBreakers, add `buffer_occupancy_pct: f32` field and `buffer_capacity: usize` field
- 1.2: Implement `update_buffer_occupancy(&mut self, current_len: usize, capacity: usize)` that computes occupancy percentage
- 1.3: At 95% occupancy: trip BufferOverflow breaker (manual reset required)
- 1.4: At 80% occupancy: trip DataQuality gate (auto-clear, pauses trade evaluation only — processing continues to drain queue)
- 1.5: At 50% occupancy: log warning, increment metric (no breaker/gate action)
- 1.6: At 100% full: log the drop explicitly, trigger immediate circuit break — never silently drop
- 1.7: Ensure 80% gate and 95% breaker are tracked as distinct states — both can be active simultaneously

### Task 2: Implement data quality gate (AC: stale/gaps/!is_tradeable, auto-clear)
- 2.1: Implement `update_data_quality(&mut self, is_tradeable: bool, has_gap: bool, is_stale: bool)`
- 2.2: When any quality condition fails (`!is_tradeable || has_gap || is_stale`), trip DataQuality gate
- 2.3: When all quality conditions pass, auto-clear the DataQuality gate
- 2.4: `is_tradeable()` comes from `OrderBook::is_tradeable()` (defined in core)
- 2.5: Staleness detection: no market data update within configurable threshold (e.g., 5 seconds during market hours)
- 2.6: Gap detection: sequence number gap or timestamp discontinuity in market data feed

### Task 3: Implement fee staleness gate (AC: >60 days, blocks trade eval, never blocks safety, auto-clear)
- 3.1: Implement `check_fee_staleness(&mut self, fee_schedule_date: chrono::NaiveDate, current_date: chrono::NaiveDate)`
- 3.2: When `current_date - fee_schedule_date > 60 days`, trip FeeStaleness gate
- 3.3: CRITICAL: fee staleness gate ONLY blocks trade evaluation (signal -> order decision). It NEVER blocks safety orders (stop-loss) or flatten orders
- 3.4: Implement `permits_trade_evaluation(&self) -> Result<(), TradingDenied>` as a separate check from `permits_trading()` to enforce this distinction
- 3.5: Auto-clears when config is updated with a fresh `fee_schedule_date`
- 3.6: Wire into fee gate module (`engine/src/risk/fee_gate.rs`) for integration with trade evaluation pipeline

### Task 4: Implement malformed message breaker (AC: sliding 60s window, >10 threshold, manual reset)
- 4.1: Add `malformed_timestamps: VecDeque<Instant>` field to CircuitBreakers
- 4.2: Implement `record_malformed_message(&mut self, timestamp: Instant)` that adds to the sliding window
- 4.3: On each call, prune entries older than 60 seconds from the front of the deque
- 4.4: When `malformed_timestamps.len() > 10` after pruning, trip MalformedMessages breaker
- 4.5: MalformedMessages is a breaker (manual reset) — feed may be corrupt, needs investigation
- 4.6: Note: individual malformed messages are already logged/skipped by `broker/src/message_validator.rs` (Story 2.1) — this breaker detects the pattern

### Task 5: Implement connection failure breaker (AC: reconnection FSM CircuitBreak state)
- 5.1: Implement `on_connection_state_change(&mut self, new_state: ConnectionState)`
- 5.2: When `new_state == ConnectionState::CircuitBreak`, trip ConnectionFailure breaker
- 5.3: ConnectionFailure is a breaker (manual reset) — state may be corrupt after failed reconciliation
- 5.4: CircuitBreak state is entered when: reconciliation times out (60s), or position mismatch detected during reconciliation
- 5.5: Wire into the reconnection FSM in `engine/src/connection/fsm.rs`

### Task 6: Wire all infrastructure breakers into event loop (AC: all checked before order)
- 6.1: Call `update_buffer_occupancy()` on each event loop iteration with current SPSC buffer stats
- 6.2: Call `update_data_quality()` after order book update with `book.is_tradeable()`, gap/staleness flags
- 6.3: Call `check_fee_staleness()` on startup and periodically (e.g., once per minute, not every tick)
- 6.4: `record_malformed_message()` is called from message validator via channel notification
- 6.5: `on_connection_state_change()` is called from reconnection FSM state transitions

### Task 7: Unit tests (AC: all)
- 7.1: Test buffer overflow breaker trips at 95%, distinct from 80% gate
- 7.2: Test 80% buffer gate auto-clears when occupancy drops below 80%
- 7.3: Test 100% full buffer triggers immediate circuit break with explicit log
- 7.4: Test data quality gate trips on `!is_tradeable()`, auto-clears when restored
- 7.5: Test data quality gate trips on stale data, auto-clears on fresh data
- 7.6: Test fee staleness gate trips at 61 days, does not trip at 60 days
- 7.7: Test fee staleness gate does NOT block safety/flatten orders (only trade evaluation)
- 7.8: Test malformed message sliding window prunes entries older than 60s
- 7.9: Test malformed message breaker trips at 11th message within 60s window
- 7.10: Test malformed message breaker does NOT trip when 10 messages spread over >60s
- 7.11: Test connection failure breaker trips on CircuitBreak FSM state
- 7.12: Test all infrastructure breakers can coexist (multiple tripped simultaneously)

## Dev Notes

### Architecture Patterns & Constraints
- All infrastructure breakers are part of the CircuitBreakers struct — single source of truth
- Buffer overflow thresholds are tiered: 50% warn, 80% disable trading (gate), 95% circuit break (breaker), 100% explicit drop + immediate break
- SPSC buffer size is 128K entries (power of 2, ~10 seconds of peak market data)
- Fee staleness gate distinction: blocks trade evaluation but NEVER blocks safety orders — this requires a separate check method
- Malformed message sliding window uses `VecDeque<Instant>` with front-pruning — efficient O(1) amortized
- Connection failure breaker is triggered by the reconnection FSM, not directly by connection loss detection
- Never block the SPSC producer — blocking risks the I/O thread falling behind Rithmic, causing disconnect
- `unsafe` is forbidden in `engine/src/risk/`

### Project Structure Notes
```
crates/engine/
├── src/
│   ├── risk/
│   │   ├── circuit_breakers.rs # Buffer overflow, data quality, malformed messages, connection failure
│   │   └── fee_gate.rs        # Fee staleness gate, trade eval gating (distinct from safety orders)
│   ├── event_loop.rs           # Wiring: buffer stats, data quality, fee check calls
│   └── connection/
│       └── fsm.rs              # Reconnection FSM — triggers connection failure breaker
crates/broker/
├── src/
│   └── message_validator.rs    # Malformed message counting (feeds into breaker via channel)
crates/core/
├── src/
│   └── order_book/
│       └── order_book.rs       # is_tradeable() — input to data quality gate
```

### References
- Architecture document: `_bmad-output/planning-artifacts/architecture.md` — Sections: SPSC Buffer Strategy, Circuit Breakers & Risk Gates, Malformed Message Handling, Reconnection FSM
- Epics document: `_bmad-output/planning-artifacts/epics.md` — Epic 5, Story 5.4
- SPSC buffer thresholds: 50% warn / 80% disable trading / 95% circuit break / 100% explicit drop (architecture.md)
- Malformed message window: sliding 60s, >10 threshold (architecture.md, Implementation Specifications)

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
