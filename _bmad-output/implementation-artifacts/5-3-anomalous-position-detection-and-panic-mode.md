# Story 5.3: Anomalous Position Detection & Panic Mode

Status: ready-for-dev

## Story

As a trader-operator,
I want the system to detect unexpected positions and escalate to panic mode if flattening fails,
So that I never have unmanaged risk exposure.

## Acceptance Criteria (BDD)

- Given position outside active strategy context When detected Then anomalous position breaker activates, automated flatten submitted
- Given `broker/src/position_flatten.rs` When flatten attempted Then market order submitted, if rejected wait 1s retry (3 attempts total), returns success/failure
- Given `engine/src/risk/panic_mode.rs` When all 3 attempts fail Then panic mode: all trading disabled, all entry/limit cancelled (stops preserved), operator alerted, requires manual restart, full context logged

## Tasks / Subtasks

### Task 1: Implement anomalous position detection (AC: position outside strategy context)
- 1.1: In `engine/src/risk/circuit_breakers.rs`, implement `check_position_anomaly(&mut self, current_position: i32, strategy_expected_position: i32)`
- 1.2: If `current_position != 0` and no active strategy expects this position, trip the AnomalousPosition breaker
- 1.3: Detection scenarios: position exists after all strategies are flat, position size differs from strategy tracking, position discovered during reconciliation with no matching bracket
- 1.4: Emit CircuitBreakerEvent with full position details in the trigger reason

### Task 2: Implement position flatten mechanism in broker (AC: flatten retry, 3 attempts, 1s interval)
- 2.1: Create `broker/src/position_flatten.rs` with `PositionFlattener` struct
- 2.2: Implement `async fn flatten(&self, symbol: &str, position_size: i32) -> Result<(), FlattenError>` that submits a market order to close the position
- 2.3: Implement retry logic: on rejection, wait 1 second, retry up to 3 total attempts
- 2.4: Define `FlattenError` enum with variants: `OrderRejected { attempt: u8, reason: String }`, `AllAttemptsFailed { attempts: Vec<String> }`
- 2.5: Return `Ok(())` on successful fill, `Err(FlattenError::AllAttemptsFailed)` after 3 failures
- 2.6: Log each attempt at `warn` level with attempt number, order details, and rejection reason if applicable

### Task 3: Implement panic mode policy (AC: all trading disabled, entries cancelled, stops preserved, alert, restart required, logged)
- 3.1: Create `engine/src/risk/panic_mode.rs` with `PanicMode` struct
- 3.2: Implement `activate(&mut self, context: PanicContext)` that transitions system to panic mode
- 3.3: Define `PanicContext` struct with fields: position_details (symbol, size, side), flatten_attempts (Vec of attempt results with rejection reasons), timestamp
- 3.4: On activation: set `is_active` flag that prevents ALL order submission (checked in `permits_trading()`)
- 3.5: On activation: collect entry/limit order IDs for cancellation, explicitly excluding stop-loss orders
- 3.6: On activation: emit alert via crossbeam channel to alerting subsystem (Story 5.6)
- 3.7: Log panic mode activation at `error` level with full PanicContext serialized

### Task 4: Wire anomaly detection into event loop (AC: detection triggers flatten, flatten failure triggers panic)
- 4.1: After position reconciliation or fill processing, call `check_position_anomaly()`
- 4.2: When anomalous position detected, spawn flatten attempt via `PositionFlattener`
- 4.3: On flatten success: log resolution, anomalous position breaker remains tripped (manual reset)
- 4.4: On flatten failure (`AllAttemptsFailed`): activate panic mode via `PanicMode::activate()`
- 4.5: Panic mode is terminal — system cannot resume without process restart

### Task 5: Integrate panic mode with CircuitBreakers (AC: panic mode checked in permits_trading)
- 5.1: Add `panic_mode: bool` field to CircuitBreakers struct
- 5.2: In `permits_trading()`, check panic_mode first — if active, return denial with `PanicModeActive` reason
- 5.3: Panic mode overrides all other breaker states — it is the highest severity

### Task 6: Unit tests (AC: all)
- 6.1: Test anomalous position detection when position exists outside strategy context
- 6.2: Test anomalous position detection does NOT trigger when strategy expects the position
- 6.3: Test flatten retry submits up to 3 attempts with 1s delays
- 6.4: Test flatten returns success on first attempt when order fills
- 6.5: Test flatten returns AllAttemptsFailed after 3 rejections
- 6.6: Test panic mode activation disables all trading via permits_trading()
- 6.7: Test panic mode cancellation list excludes stop-loss orders
- 6.8: Test panic mode logs full context including all flatten attempt details
- 6.9: Test panic mode cannot be reset (requires restart)
- 6.10: Test full flow: anomaly detected -> flatten fails -> panic mode activates

## Dev Notes

### Architecture Patterns & Constraints
- Anomalous position = breaker (manual reset) — needs investigation even if flatten succeeds
- Flatten retry: 3 attempts, 1 second between each. This is the ONLY place in the codebase with intentional delays
- Panic mode is the ultimate escalation — system is in an unrecoverable state and cannot resume without manual restart
- CRITICAL: stops ALWAYS preserved during panic mode. Only entry/limit orders are cancelled
- The flatten mechanism lives in broker crate (it submits orders), the panic mode policy lives in engine crate (it decides what to do)
- Separation of concerns: `broker/position_flatten.rs` handles the mechanics (submit + retry), `engine/risk/panic_mode.rs` handles the policy (when to flatten, what to do on failure)
- Flatten uses async (tokio::time::sleep for 1s delays) since it runs on the I/O thread, not the hot path
- `unsafe` is forbidden in both `engine/src/risk/` and `broker/src/position_flatten.rs`

### Project Structure Notes
```
crates/broker/
├── src/
│   └── position_flatten.rs     # PositionFlattener: submit market order, retry 3x with 1s interval
crates/engine/
├── src/
│   ├── risk/
│   │   ├── circuit_breakers.rs # check_position_anomaly(), panic_mode flag
│   │   └── panic_mode.rs       # PanicMode struct, PanicContext, activate()
│   └── event_loop.rs           # Wiring: anomaly check -> flatten -> panic escalation
```

### References
- Architecture document: `_bmad-output/planning-artifacts/architecture.md` — Section: Circuit Breakers (anomalous position), Bracket Order Lifecycle (flatten retry -> panic mode)
- Epics document: `_bmad-output/planning-artifacts/epics.md` — Epic 5, Story 5.3
- Spec: Flatten retry count + interval — 3 attempts, 1s between (architecture.md, Implementation Specifications table)
- Spec: Panic mode definition — disable all trading, cancel entries (preserve stops), alert operator, require manual restart

## Dev Agent Record

### Agent Model Used
### Debug Log References
### Completion Notes List
### File List
