# Story 9.2: Diagnostic Event Logging

Status: ready-for-dev

## Story

As a trader-operator,
I want detailed diagnostic logs for circuit breaker and regime events,
So that I can understand exactly why the system stopped trading or changed behavior.

## Acceptance Criteria (BDD)

- Given circuit breaker activates When logged Then includes: breaker type, trigger condition, threshold value, actual value, timestamp, position state, P&L, all other breaker states
- Given regime transition When logged Then includes: from state, to state, timestamp, contributing indicator values (ATR, directional persistence), enables post-hoc analysis
- Given system state changes (connection, strategy enable/disable) When logged Then full diagnostic context, state transitions form coherent timeline

## Tasks / Subtasks

### Task 1: Implement circuit breaker diagnostic logging (AC: breaker activation includes full context)
- 1.1: In `engine/src/risk/circuit_breakers.rs`, add structured `tracing::warn!` in `trip_breaker()` with fields: `breaker_type`, `trigger_reason`, `threshold`, `actual_value`, `timestamp`
- 1.2: Include current position state fields in the log: `position_side`, `position_quantity`, `unrealized_pnl_ticks`
- 1.3: Include current daily P&L: `daily_pnl_ticks`, `trade_count`
- 1.4: Snapshot and include all other breaker states as a structured field (e.g., `breaker_states = "{daily_loss: Active, consecutive: Tripped, ...}"`)
- 1.5: Log at `warn` level for gate trips, `error` level for breaker trips

### Task 2: Implement circuit breaker clearance logging (AC: coherent timeline)
- 2.1: In `clear_gate()`, add structured `tracing::info!` with fields: `gate_type`, `timestamp`, `duration_tripped_ms` (time since trip)
- 2.2: In `reset_breaker()`, add structured `tracing::info!` with fields: `breaker_type`, `timestamp`, `reset_source` ("manual" or "restart")
- 2.3: Ensure trip and clear events use the same `breaker_type` field name for log correlation

### Task 3: Implement regime transition diagnostic logging (AC: from/to state, indicator values)
- 3.1: In `engine/src/regime/threshold.rs`, add structured `tracing::info!` when `RegimeState` changes
- 3.2: Include fields: `from_regime` (previous state), `to_regime` (new state), `timestamp`
- 3.3: Include contributing indicator values: `atr_value`, `atr_threshold`, `directional_persistence`, `dp_threshold`
- 3.4: Include any additional regime-relevant metrics: `lookback_bars`, `sample_count`
- 3.5: Log at `info` level — regime transitions are expected operational events, not errors

### Task 4: Implement RegimeTransition event struct if not already present (AC: enables post-hoc analysis)
- 4.1: Verify `core/src/events/risk.rs` has `RegimeTransition` struct with fields: `from` (RegimeState), `to` (RegimeState), `timestamp` (UnixNanos), `indicator_values` (struct or map)
- 4.2: If not present, define `RegimeTransitionIndicators` struct: `atr: f64`, `directional_persistence: f64`, plus optional additional fields
- 4.3: Emit `RegimeTransition` event to journal channel alongside the log entry for persistence

### Task 5: Implement connection state change diagnostic logging (AC: system state changes with full context)
- 5.1: In `broker/src/connection.rs` (or `engine/src/connection/fsm.rs`), add structured `tracing::info!` on every FSM state transition
- 5.2: Include fields: `from_state`, `to_state`, `timestamp`, `reason` (e.g., "heartbeat timeout", "reconnect success")
- 5.3: For connection failures, log at `error` level with `error_detail`, `retry_count`, `backoff_ms`
- 5.4: For successful reconnections, log at `info` level with `downtime_ms`

### Task 6: Implement strategy enable/disable logging (AC: system state changes with full context)
- 6.1: When strategy is enabled or disabled (e.g., regime change disabling trading), log at `info` level
- 6.2: Include fields: `strategy_name`, `enabled` (bool), `reason` (e.g., "regime transition to Volatile"), `timestamp`
- 6.3: Include current regime state and active breaker count for context

### Task 7: Ensure coherent timeline across all diagnostic events (AC: coherent timeline in log stream)
- 7.1: All diagnostic events use the same timestamp source (injected `Clock` for event timestamps, tracing subscriber for log timestamps)
- 7.2: Verify that log entries appear in chronological order when tailing with `journalctl`
- 7.3: Add `event_type` field to all diagnostic logs for easy filtering: `"circuit_breaker"`, `"regime_transition"`, `"connection_state"`, `"strategy_state"`
- 7.4: Document recommended filter queries: `jq 'select(.event_type == "circuit_breaker")'`

### Task 8: Unit tests (AC: all)
- 8.1: Test circuit breaker trip logging includes all required fields (capture tracing output with `tracing_test` or subscriber mock)
- 8.2: Test regime transition logging includes from/to states and indicator values
- 8.3: Test connection state change logging includes FSM states and timing
- 8.4: Test that gate clearance log includes duration_tripped_ms
- 8.5: Test all diagnostic logs have consistent `event_type` field

## Dev Notes

### Architecture Patterns & Constraints
- Circuit breaker events must include ALL context needed for post-incident analysis: the breaker that tripped, the values that triggered it, AND the state of all other breakers
- Regime transitions are operational events (info level), not errors — the system is working as designed when it transitions
- All diagnostic logging uses structured fields (key-value pairs), never interpolated format strings, so they are machine-parseable in production JSON mode
- Connection state changes follow the 5-state reconnection FSM defined in `engine/src/connection/fsm.rs`
- `CircuitBreakerEvent` and `RegimeTransition` are emitted both as structured logs AND as journal events (dual-write: log for real-time tailing, journal for persistence)
- Hot path constraint still applies: diagnostic events sent via crossbeam channel, log emission is non-blocking

### Project Structure Notes
```
crates/core/
├── src/
│   └── events/
│       └── risk.rs             # CircuitBreakerEvent, RegimeTransition structs
crates/engine/
├── src/
│   ├── risk/
│   │   ├── circuit_breakers.rs # Trip/clear logging with full breaker state snapshot
│   │   └── mod.rs              # TradingDenied logging
│   ├── regime/
│   │   └── threshold.rs        # Regime transition diagnostic logging
│   └── connection/
│       └── fsm.rs              # Connection state change logging
crates/broker/
├── src/
│   └── connection.rs           # Low-level connection event logging
```

### References
- Architecture document: `_bmad-output/planning-artifacts/architecture.md` — Sections: Circuit Breakers & Risk Gates, Structured Logging
- Epics document: `_bmad-output/planning-artifacts/epics.md` — Epic 9, Story 9.2
- Story 5.1: Circuit Breaker Framework — defines CircuitBreakerEvent, BreakerType, BreakerState
- Story 9.1: Structured Logging — prerequisite, provides tracing infrastructure
- Dependencies: tracing 0.1.44

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
