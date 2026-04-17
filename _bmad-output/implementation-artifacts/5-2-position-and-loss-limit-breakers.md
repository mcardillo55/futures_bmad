# Story 5.2: Position & Loss Limit Breakers

Status: ready-for-dev

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
- 1.1: Add `max_position_size: u32` field to CircuitBreakers (from TradingConfig)
- 1.2: Implement `check_position_size(&mut self, current_position: i32)` that compares `abs(current_position)` against `max_position_size`
- 1.3: When `abs(current_position) >= max_position_size`, trip the MaxPositionSize gate
- 1.4: When `abs(current_position) < max_position_size`, auto-clear the gate (emit clearance event)
- 1.5: Integrate into `update_gate_conditions()` so it is re-evaluated each tick

### Task 2: Implement daily loss breaker (AC: daily loss, session over, manual reset, persists)
- 2.1: Add `max_daily_loss_ticks: i64` and `daily_loss_current: i64` fields to CircuitBreakers
- 2.2: Implement `update_daily_loss(&mut self, realized_pnl: i64, unrealized_pnl: i64)` that computes cumulative loss in quarter-ticks
- 2.3: When `daily_loss_current` exceeds `max_daily_loss_ticks` (as negative value), trip the DailyLoss breaker
- 2.4: DailyLoss is a breaker (manual reset = restart) — never auto-clears even if P&L recovers
- 2.5: Ensure breaker state persists across reconnections: once tripped, stays tripped until process restart
- 2.6: Track both realized and unrealized P&L: `daily_loss_current = realized_pnl + unrealized_pnl`

### Task 3: Implement consecutive loss breaker (AC: consecutive losses, counter reset on win)
- 3.1: Add `max_consecutive_losses: u32` and `consecutive_loss_count: u32` fields to CircuitBreakers
- 3.2: Implement `record_trade_result(&mut self, is_winner: bool)` that updates the consecutive loss counter
- 3.3: On losing trade: increment `consecutive_loss_count`; when it reaches `max_consecutive_losses`, trip the ConsecutiveLosses breaker
- 3.4: On winning trade: reset `consecutive_loss_count` to 0 — this happens regardless of breaker state
- 3.5: IMPORTANT: counter resets on a winning trade, NOT on breaker reset. If breaker is manually reset but next trade is a loss, the counter continues from where the winning trade reset it

### Task 4: Implement max trades per day breaker (AC: max trades, manual reset)
- 4.1: Add `max_trades_per_day: u32` and `trade_count: u32` fields to CircuitBreakers
- 4.2: Implement `record_trade(&mut self)` that increments `trade_count`
- 4.3: When `trade_count > max_trades_per_day`, trip the MaxTrades breaker
- 4.4: MaxTrades is a breaker (manual reset = restart) — count never resets within a session

### Task 5: Wire breakers into event loop (AC: all breakers checked before order)
- 5.1: In `event_loop.rs`, after trade decision and before order submission, call `permits_trading()`
- 5.2: Call `update_daily_loss()` after each fill or position mark-to-market
- 5.3: Call `record_trade_result()` after each trade completes (fill received)
- 5.4: Call `record_trade()` after each order submitted
- 5.5: Call `check_position_size()` with current position size after each position change

### Task 6: Unit tests (AC: all)
- 6.1: Test max position size gate blocks when at limit, auto-clears when position reduces
- 6.2: Test daily loss breaker trips when cumulative loss exceeds threshold
- 6.3: Test daily loss breaker does NOT auto-clear even if P&L recovers
- 6.4: Test daily loss includes both realized and unrealized components
- 6.5: Test consecutive loss counter increments on losses, resets on win
- 6.6: Test consecutive loss breaker trips at exact threshold (e.g., 5th consecutive loss when max is 5)
- 6.7: Test consecutive loss counter resets on winning trade even when breaker is tripped
- 6.8: Test max trades breaker trips when count exceeded
- 6.9: Test all four breakers can be tripped simultaneously, `permits_trading()` returns all denial reasons
- 6.10: Test configuration with various threshold values from TradingConfig

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
### Debug Log References
### Completion Notes List
### File List
