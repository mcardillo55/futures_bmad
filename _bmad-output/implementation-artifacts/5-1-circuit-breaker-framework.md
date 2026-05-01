# Story 5.1: Circuit Breaker Framework

Status: review

## Story

As a trader-operator,
I want a unified risk management framework checked before every order,
So that no trade can bypass safety checks regardless of market conditions.

## Acceptance Criteria (BDD)

- Given `engine/src/risk/circuit_breakers.rs` When CircuitBreakers struct implemented Then single source of truth for "can we trade?", contains state for all gate/breaker types, distinguishes gates (auto-clear) from breakers (manual reset)
- Given framework When `permits_trading()` called Then ALL breakers/gates checked synchronously, returns `Result` with specific denial reasons, checking within one tick (NFR10)
- Given breaker tripped When activates Then entry/limit orders cancelled, stop-loss orders PRESERVED, `CircuitBreakerEvent` emitted to journal
- Given gate condition resolves When auto-clears Then trading resumes, clearance logged

## Tasks / Subtasks

### Task 1: Define CircuitBreakerEvent and related types in core (AC: event emission)
- [x] 1.1: In `core/src/events/risk.rs`, define `CircuitBreakerEvent` struct with fields: breaker_type (`BreakerType` enum), trigger_reason (String), timestamp (`UnixNanos`), previous_state, new_state
- [x] 1.2: Define `BreakerType` enum with all 10 variants: `DailyLoss`, `ConsecutiveLosses`, `MaxTrades`, `AnomalousPosition`, `ConnectionFailure`, `BufferOverflow`, `MaxPositionSize`, `DataQuality`, `FeeStaleness`, `MalformedMessages`
- [x] 1.3: Define `BreakerCategory` enum: `Gate` (auto-clear), `Breaker` (manual reset)
- [x] 1.4: Define `BreakerState` enum: `Active`, `Tripped`, `Cleared`
- [x] 1.5: Implement `BreakerType::category(&self) -> BreakerCategory` to classify each type

### Task 2: Define TradingConfig risk limit fields in core (AC: configuration)
- [x] 2.1: In `core/src/config/trading.rs`, ensure `TradingConfig` has fields: `max_position_size` (u32), `max_daily_loss_ticks` (i64), `max_consecutive_losses` (u32), `max_trades_per_day` (u32)
- [x] 2.2: Add `fee_schedule_date` field (chrono::NaiveDate) to config for staleness checking
- [x] 2.3: Add serde Deserialize derives for TOML loading

### Task 3: Define TradingDenied error type (AC: specific denial reasons)
- [x] 3.1: Define `TradingDenied` struct in `engine/src/risk/mod.rs` containing a `Vec<DenialReason>`
- [x] 3.2: Define `DenialReason` enum with variants matching each breaker/gate type, each carrying context (e.g., `DailyLossExceeded { current: i64, limit: i64 }`)
- [x] 3.3: Implement `Display` for `TradingDenied` listing all active denial reasons

### Task 4: Implement CircuitBreakers struct (AC: single source of truth, gate vs breaker distinction)
- [x] 4.1: Create `engine/src/risk/circuit_breakers.rs` with `CircuitBreakers` struct
- [x] 4.2: Add state fields for all 10 breaker/gate types, each as a `BreakerState`
- [x] 4.3: Add associated data fields: `daily_loss_current` (i64), `consecutive_loss_count` (u32), `trade_count` (u32), `malformed_window` (VecDeque<Instant>), `buffer_occupancy_pct` (f32)
- [x] 4.4: Implement `CircuitBreakers::new(config: &TradingConfig) -> Self` initializing all breakers to `Active`

### Task 5: Implement permits_trading() (AC: synchronous check, denial reasons, NFR10)
- [x] 5.1: Implement `permits_trading(&self) -> Result<(), TradingDenied>` that checks ALL breaker/gate states
- [x] 5.2: Collect all active denial reasons into a single `TradingDenied` — do not short-circuit on first failure
- [x] 5.3: Ensure method is `#[inline]` and does no heap allocation on the happy path (all checks are field comparisons)
- [x] 5.4: Return `Ok(())` only when every breaker and gate is in `Active` state

### Task 6: Implement breaker activation logic (AC: cancel entries, preserve stops, emit event)
- [x] 6.1: Implement `trip_breaker(&mut self, breaker_type: BreakerType, reason: String, timestamp: UnixNanos) -> CircuitBreakerEvent`
- [x] 6.2: Implement `clear_gate(&mut self, gate_type: BreakerType, timestamp: UnixNanos) -> CircuitBreakerEvent` that only works for gate-category types
- [x] 6.3: Implement `orders_to_cancel(&self, active_orders: &[Order]) -> Vec<OrderId>` that returns entry/limit order IDs, filtering out stop-loss orders
- [x] 6.4: Implement `reset_breaker(&mut self, breaker_type: BreakerType)` for manual reset (used on restart)

### Task 7: Wire CircuitBreakerEvent emission (AC: journal emission)
- [x] 7.1: Accept a `crossbeam_channel::Sender<CircuitBreakerEvent>` in CircuitBreakers constructor
- [x] 7.2: In `trip_breaker()` and `clear_gate()`, send event through the channel
- [x] 7.3: Ensure send is non-blocking (use `try_send`, log if channel full but never block)

### Task 8: Implement gate auto-clear mechanism (AC: auto-clear, logged)
- [x] 8.1: Implement `update_gate_conditions(&mut self, ...)` that re-evaluates gate conditions each tick
- [x] 8.2: When a gate transitions from `Tripped` to `Active`, emit a `CircuitBreakerEvent` with cleared state
- [x] 8.3: Log gate clearance at `info` level with gate type and duration it was tripped

### Task 9: Unit tests (AC: all)
- [x] 9.1: Test `permits_trading()` returns `Ok` when all breakers/gates active
- [x] 9.2: Test `permits_trading()` returns `Err` with correct denial reasons when breakers tripped
- [x] 9.3: Test `trip_breaker()` emits `CircuitBreakerEvent` on channel
- [x] 9.4: Test `orders_to_cancel()` preserves stop-loss orders and returns only entry/limit orders
- [x] 9.5: Test gate auto-clear transitions from `Tripped` to `Active`
- [x] 9.6: Test breaker manual reset requires explicit call (does not auto-clear)
- [x] 9.7: Test multiple simultaneous breakers produce multiple denial reasons

## Dev Notes

### Architecture Patterns & Constraints
- CircuitBreakers is the single source of truth for "can we trade?" — checked synchronously before every order submission in the event loop
- Gate vs Breaker distinction: gates auto-clear when condition resolves, breakers require manual reset (= process restart in V1)
- 10 breaker/gate types total: 6 breakers (daily loss, consecutive losses, max trades, anomalous position, connection failure, buffer overflow, malformed messages) and 3 gates (max position size, data quality, fee staleness)
- CRITICAL: circuit break preserves stop-loss orders. Only cancel entry/limit orders. Never cancel a stop unless flattening the associated position with a market order first
- NFR10: checking must complete within one tick — no deferred checks, no async
- `unsafe` is forbidden in `engine/src/risk/`
- No heap allocation on the hot path (`permits_trading()` happy path)
- CircuitBreakerEvent sent via crossbeam channel to journal (async write, never blocks hot path)

### Project Structure Notes
```
crates/core/
├── src/
│   ├── events/
│   │   └── risk.rs             # CircuitBreakerEvent, BreakerType, BreakerState
│   └── config/
│       └── trading.rs          # TradingConfig with risk limit fields
crates/engine/
├── src/
│   ├── risk/
│   │   ├── mod.rs              # CircuitBreakers struct, TradingDenied, DenialReason
│   │   └── circuit_breakers.rs # All breaker/gate state, permits_trading(), trip/clear logic
│   └── event_loop.rs           # Calls permits_trading() before every order submission
```

### References
- Architecture document: `_bmad-output/planning-artifacts/architecture.md` — Section: Circuit Breakers & Risk Gates, Communication & Error Handling
- Epics document: `_bmad-output/planning-artifacts/epics.md` — Epic 5, Story 5.1
- Dependencies: crossbeam-channel 0.5.15, tracing 0.1.44, thiserror 2.0.18

## Dev Agent Record

### Agent Model Used
Claude Opus 4.7 (1M context) — `claude-opus-4-7[1m]`

### Debug Log References
- `cargo build -p futures_bmad_engine` — compiled clean after `Eq` derive removal on `DenialReason`/`TradingDenied` (the `BufferOverflow { occupancy_pct: f32 }` variant carries an `f32` which is not `Eq`).
- `cargo test --workspace` — 329 tests passing; new file `circuit_breakers.rs` contributes 10 unit tests; `events/risk.rs` adds 2 more.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo fmt --all` — applied; pre-existing diffs in unrelated files (vpin_tests.rs, levels_tests.rs, etc.) were also normalized.

### Completion Notes List
- Replaced the placeholder `CircuitBreakerType` (5 variants) and `CircuitBreakerEvent` (with `triggered: bool`) shipped by story 1.6 with the story-5.1 spec shape: `BreakerType` (10 variants), `BreakerCategory`, `BreakerState`, and the `previous_state -> new_state` transition fields on `CircuitBreakerEvent`. The old type was not referenced outside its module so no downstream call-sites needed updating beyond the `EngineEvent::CircuitBreaker(CircuitBreakerEvent)` enum, which already wraps the event by reference.
- `TradingConfig` field swap: replaced `max_daily_loss: FixedPrice` with `max_daily_loss_ticks: i64` (architecture spec uses tick-denominated caps in the risk framework), and added `max_trades_per_day: u32` and `fee_schedule_date: NaiveDate`. Validation tightened (`ZeroMaxTradesPerDay` error variant) and the test fixture was updated.
- `CircuitBreakers::permits_trading()` is `#[inline]` and short-circuits to `Ok(())` via `all_active()`, which is a fixed boolean reduction over ten `matches!(state, BreakerState::Active)` checks — zero heap allocation on the happy path. Failure-side construction allocates a `Vec<DenialReason>` (cap 10) only when at least one breaker / gate is non-`Active`, which is acceptable because that path is the cold path by definition.
- `orders_to_cancel()` filters `OrderType::Stop { .. }` out, returning only entry market orders and resting limit order IDs. Verified by Task 9.4's unit test which feeds all three order types and asserts only the entry + limit IDs are returned.
- `clear_gate()` enforces gate-only semantics by returning `GateClearError::NotAGate(t)` if called with a manual-reset breaker type; this prevents an accidental "clear" from circumventing the manual-reset invariant. `update_gate_conditions()` is the public re-evaluation entry point — only the three gate types (`MaxPositionSize`, `DataQuality`, `FeeStaleness`) are auto-cleared.
- `CircuitBreakerEvent` emission uses `try_send`; on `Full`/`Disconnected` we log and drop (matches the journal policy elsewhere in the engine — never block the hot path on a journal write).
- `unsafe_code` is denied at the file level in both `engine/src/risk/mod.rs` and `engine/src/risk/circuit_breakers.rs`, satisfying the architecture-spec constraint that `unsafe` is forbidden in `engine/src/risk/`.
- Stories 5.2–5.6 will plug into the `GateConditions` / `trip_breaker` / `reset_breaker` API surface; nothing in that surface is changeable without coordinated downstream updates.

### File List
- crates/core/src/events/risk.rs — replaced (BreakerType, BreakerCategory, BreakerState, CircuitBreakerEvent)
- crates/core/src/events/mod.rs — re-exports updated
- crates/core/src/lib.rs — re-exports updated
- crates/core/src/config/trading.rs — added max_daily_loss_ticks, max_trades_per_day, fee_schedule_date; removed max_daily_loss
- crates/core/src/config/validation.rs — ZeroMaxTradesPerDay variant; updated daily-loss check; updated test fixture
- crates/engine/src/risk/mod.rs — TradingDenied, DenialReason, Display impl; module-level deny(unsafe_code)
- crates/engine/src/risk/circuit_breakers.rs — CircuitBreakers, GateConditions, GateClearError + 10 unit tests (NEW)
- _bmad-output/implementation-artifacts/sprint-status.yaml — story marked review
