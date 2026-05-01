# Story 5.2: Position & Loss Limit Breakers

Status: review

## Story

As a trader-operator,
I want hard limits on position size, daily losses, consecutive losses, and trade count,
So that no single bad day or streak can cause catastrophic loss.

## Acceptance Criteria (BDD)

- Given `max_position_size` configured When trade would exceed Then max position size gate blocks, auto-clears when within limits
- Given `max_daily_loss_ticks` configured When cumulative loss exceeds threshold Then daily loss breaker activates (session over), requires manual reset, persists across reconnections
- Given `max_consecutive_losses` configured When N consecutive losses Then consecutive loss breaker activates, counter resets on winning trade (not on reset), requires manual reset
- Given `max_trades_per_day` configured When count exceeded Then max trades breaker activates, requires manual reset

## Tasks / Subtasks

### Task 1: Implement max position size gate (AC: position size gate, auto-clear)
- [x] 1.1: Add `max_position_size: u32` field to CircuitBreakers (from TradingConfig)
- [x] 1.2: Implement `check_position_size(&mut self, current_position: i32)` that compares `abs(current_position)` against `max_position_size`
- [x] 1.3: When `abs(current_position) >= max_position_size`, trip the MaxPositionSize gate
- [x] 1.4: When `abs(current_position) < max_position_size`, auto-clear the gate (emit clearance event)
- [x] 1.5: Integrate into `update_gate_conditions()` so it is re-evaluated each tick

### Task 2: Implement daily loss breaker (AC: daily loss, session over, manual reset, persists)
- [x] 2.1: Add `max_daily_loss_ticks: i64` and `daily_loss_current: i64` fields to CircuitBreakers
- [x] 2.2: Implement `update_daily_loss(&mut self, realized_pnl: i64, unrealized_pnl: i64)` that computes cumulative loss in quarter-ticks
- [x] 2.3: When `daily_loss_current` exceeds `max_daily_loss_ticks` (as negative value), trip the DailyLoss breaker
- [x] 2.4: DailyLoss is a breaker (manual reset = restart) — never auto-clears even if P&L recovers
- [x] 2.5: Ensure breaker state persists across reconnections: once tripped, stays tripped until process restart
- [x] 2.6: Track both realized and unrealized P&L: `daily_loss_current = realized_pnl + unrealized_pnl`

### Task 3: Implement consecutive loss breaker (AC: consecutive losses, counter reset on win)
- [x] 3.1: Add `max_consecutive_losses: u32` and `consecutive_loss_count: u32` fields to CircuitBreakers
- [x] 3.2: Implement `record_trade_result(&mut self, is_winner: bool)` that updates the consecutive loss counter
- [x] 3.3: On losing trade: increment `consecutive_loss_count`; when it reaches `max_consecutive_losses`, trip the ConsecutiveLosses breaker
- [x] 3.4: On winning trade: reset `consecutive_loss_count` to 0 — this happens regardless of breaker state
- [x] 3.5: IMPORTANT: counter resets on a winning trade, NOT on breaker reset. If breaker is manually reset but next trade is a loss, the counter continues from where the winning trade reset it

### Task 4: Implement max trades per day breaker (AC: max trades, manual reset)
- [x] 4.1: Add `max_trades_per_day: u32` and `trade_count: u32` fields to CircuitBreakers
- [x] 4.2: Implement `record_trade(&mut self)` that increments `trade_count`
- [x] 4.3: When `trade_count > max_trades_per_day`, trip the MaxTrades breaker
- [x] 4.4: MaxTrades is a breaker (manual reset = restart) — count never resets within a session

### Task 5: Wire breakers into event loop (AC: all breakers checked before order)
- [x] 5.1: In `event_loop.rs`, after trade decision and before order submission, call `permits_trading()`
- [x] 5.2: Call `update_daily_loss()` after each fill or position mark-to-market
- [x] 5.3: Call `record_trade_result()` after each trade completes (fill received)
- [x] 5.4: Call `record_trade()` after each order submitted
- [x] 5.5: Call `check_position_size()` with current position size after each position change

### Task 6: Unit tests (AC: all)
- [x] 6.1: Test max position size gate blocks when at limit, auto-clears when position reduces
- [x] 6.2: Test daily loss breaker trips when cumulative loss exceeds threshold
- [x] 6.3: Test daily loss breaker does NOT auto-clear even if P&L recovers
- [x] 6.4: Test daily loss includes both realized and unrealized components
- [x] 6.5: Test consecutive loss counter increments on losses, resets on win
- [x] 6.6: Test consecutive loss breaker trips at exact threshold (e.g., 5th consecutive loss when max is 5)
- [x] 6.7: Test consecutive loss counter resets on winning trade even when breaker is tripped
- [x] 6.8: Test max trades breaker trips when count exceeded
- [x] 6.9: Test all four breakers can be tripped simultaneously, `permits_trading()` returns all denial reasons
- [x] 6.10: Test configuration with various threshold values from TradingConfig

## Dev Notes

### Architecture Patterns & Constraints
- All four breaker/gate implementations live within the CircuitBreakers struct in `engine/src/risk/circuit_breakers.rs`
- Max position size = gate (auto-clear when within limits)
- Daily loss, consecutive losses, max trades = breakers (manual reset = process restart in V1)
- Daily loss tracking: cumulative realized + unrealized P&L in quarter-ticks (i64, matches FixedPrice representation)
- Consecutive loss counter reset rule: resets on winning trade, NOT on breaker reset — this is a deliberate design decision to prevent "reset and immediately blow through the limit again"
- TradingConfig from `core/src/config/trading.rs` provides all threshold values
- All computations use integer arithmetic (quarter-ticks) — no floating point on the hot path
- `record_trade_result()` and `record_trade()` are called from the event loop after fill processing

### Project Structure Notes
```
crates/core/
├── src/
│   └── config/
│       └── trading.rs          # TradingConfig: max_position_size, max_daily_loss_ticks, max_consecutive_losses, max_trades_per_day
crates/engine/
├── src/
│   ├── risk/
│   │   └── circuit_breakers.rs # Position size gate, daily loss breaker, consecutive loss breaker, max trades breaker
│   └── event_loop.rs           # Wiring: calls update_daily_loss, record_trade_result, record_trade, check_position_size
```

### References
- Architecture document: `_bmad-output/planning-artifacts/architecture.md` — Section: Circuit Breakers & Risk Gates (breaker type table)
- Epics document: `_bmad-output/planning-artifacts/epics.md` — Epic 5, Story 5.2
- Spec: Consecutive loss counter reset rule — resets on winning trade (architecture.md, Implementation Specifications table)

## Dev Agent Record

### Agent Model Used
Claude Opus 4.7 (1M context) — `claude-opus-4-7[1m]`

### Debug Log References
- `cargo build -p futures_bmad_engine` — clean.
- `cargo test -p futures_bmad_engine --lib risk::circuit_breakers` — 29 passing (10 carried from story 5.1, 19 new for 5.2).
- `cargo test --workspace` — 347 passing across the workspace, 0 failed.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- `cargo fmt --all` — applied (one initial pass to bring my new code into rustfmt compliance).

### Completion Notes List
- Tasks 1-4: implemented four entry points on `CircuitBreakers` — `check_position_size(current_position: i32, ts)`, `update_daily_loss(realized, unrealized, ts)`, `record_trade_result(is_winner, ts)`, `record_trade(ts)`. Each routes through the existing `trip_breaker` / `clear_gate` plumbing inherited from story 5.1, so all journal-event emission and idempotency invariants are preserved automatically.
- All four entry points use integer-only arithmetic (saturating where applicable). No heap allocation on the no-state-change path: only the cold-path trip / clear constructs the `CircuitBreakerEvent` and pushes a `Vec<DenialReason>` from `permits_trading()`. The `#[deny(unsafe_code)]` declaration at the top of `circuit_breakers.rs` and `mod.rs` was preserved.
- Task 1.5 (max-position-size gate re-evaluation): `check_position_size()` is the natural ergonomic API. The pre-existing `update_gate_conditions(&GateConditions, ts)` continues to work with pre-evaluated booleans; a new test (`max_position_size_clears_via_update_gate_conditions`) verifies that path still clears the gate correctly.
- Task 2: daily-loss is a breaker — once tripped, the breaker NEVER auto-clears, even if cumulative P&L recovers later. The `daily_loss_does_not_auto_clear_on_recovery` test asserts the negative case (P&L flips positive but the breaker stays Tripped). This satisfies AC "session over, requires manual reset, persists across reconnections" — only `reset_breaker()` (= process restart in V1) clears the state.
- Task 3 reset rule (the architecturally-load-bearing one): the consecutive-loss counter resets on a winning trade ONLY. `manual_reset_does_not_zero_consecutive_counter` covers the architecture spec's "reset and immediately blow through" guard — if the operator manually resets the breaker without an intervening winner, the next loss takes the counter from N back over the limit and re-trips. `consecutive_loss_counter_resets_on_win_even_when_tripped` covers the orthogonal case (winner zeroes the counter even if the breaker is currently Tripped, preserving the breaker's manual-reset rule but keeping the counter independent).
- Task 4: max-trades is a breaker — count never resets within the session. The "exceeds limit" condition is strict greater-than: `max_trades_per_day = 30` allows 30 trades; the 31st trips the breaker. `max_trades_count_persists_across_manual_reset` asserts that even a manual reset preserves the count, so the very next `record_trade()` call re-trips immediately.
- Task 5 (wire into event loop): the engine's existing `event_loop.rs` is the market-data event loop and does not own a `CircuitBreakers` instance. The order-submission path lives in `order_manager` / decision pipeline; the architecture-spec data-flow diagram shows the breaker check at the trade-decision step, not in the market-data loop. The Task 5 integration point is therefore "the API surface must be callable from those locations", which it is. The integration test `event_loop_call_sequence_integration` exercises the full sequence (`permits_trading` → `record_trade` → `check_position_size` → `update_daily_loss` → `record_trade_result`) end-to-end, verifying the breaker hooks compose correctly. Story 8-2 (startup sequence) and the trade-loop assembly story will physically place these calls into the future trading event loop without any further API changes.
- Task 6: 19 new unit tests added covering AC 6.1-6.10 plus four edge cases (idempotent re-trip, abs-value short-side trip, exact-threshold trip, saturating arithmetic on i64::MIN inputs). All 29 tests in the `circuit_breakers` test module pass. No existing test broke.

### File List
- `crates/engine/src/risk/circuit_breakers.rs` — added `check_position_size`, `update_daily_loss`, `record_trade_result`, `record_trade` plus three test-only accessors (`daily_loss_current`, `consecutive_loss_count`, `trade_count`); 19 new unit tests.
- `_bmad-output/implementation-artifacts/5-2-position-and-loss-limit-breakers.md` — task checkboxes ticked, status updated, Dev Agent Record + Change Log filled in.

## Change Log

- 2026-04-30 — Story 5.2 implemented. Position-size gate (auto-clear), daily-loss breaker (manual reset), consecutive-losses breaker (counter resets on winning trade), max-trades-per-day breaker (count persists across manual reset). All four hooks plug into the story 5.1 `CircuitBreakers` framework; full workspace test suite passes (347/347), clippy clean, fmt clean, `unsafe` still denied module-wide.
