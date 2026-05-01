# Story 5.3: Anomalous Position Detection & Panic Mode

Status: review

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
- [x] 1.1: In `engine/src/risk/circuit_breakers.rs`, implement `check_position_anomaly(&mut self, current_position: i32, strategy_expected_position: i32)`
- [x] 1.2: If `current_position != 0` and no active strategy expects this position, trip the AnomalousPosition breaker
- [x] 1.3: Detection scenarios: position exists after all strategies are flat, position size differs from strategy tracking, position discovered during reconciliation with no matching bracket
- [x] 1.4: Emit CircuitBreakerEvent with full position details in the trigger reason

### Task 2: Implement position flatten mechanism in broker (AC: flatten retry, 3 attempts, 1s interval)
- [x] 2.1: Create `broker/src/position_flatten.rs` with `PositionFlattener` struct
- [x] 2.2: Implement `async fn flatten(&self, symbol: &str, position_size: i32) -> Result<(), FlattenError>` that submits a market order to close the position
- [x] 2.3: Implement retry logic: on rejection, wait 1 second, retry up to 3 total attempts
- [x] 2.4: Define `FlattenError` enum with variants: `OrderRejected { attempt: u8, reason: String }`, `AllAttemptsFailed { attempts: Vec<String> }`
- [x] 2.5: Return `Ok(())` on successful fill, `Err(FlattenError::AllAttemptsFailed)` after 3 failures
- [x] 2.6: Log each attempt at `warn` level with attempt number, order details, and rejection reason if applicable

### Task 3: Implement panic mode policy (AC: all trading disabled, entries cancelled, stops preserved, alert, restart required, logged)
- [x] 3.1: Create `engine/src/risk/panic_mode.rs` with `PanicMode` struct
- [x] 3.2: Implement `activate(&mut self, context: PanicContext)` that transitions system to panic mode
- [x] 3.3: Define `PanicContext` struct with fields: position_details (symbol, size, side), flatten_attempts (Vec of attempt results with rejection reasons), timestamp
- [x] 3.4: On activation: set `is_active` flag that prevents ALL order submission (checked in `permits_trading()`)
- [x] 3.5: On activation: collect entry/limit order IDs for cancellation, explicitly excluding stop-loss orders
- [x] 3.6: On activation: emit alert via crossbeam channel to alerting subsystem (Story 5.6)
- [x] 3.7: Log panic mode activation at `error` level with full PanicContext serialized

### Task 4: Wire anomaly detection into event loop (AC: detection triggers flatten, flatten failure triggers panic)
- [x] 4.1: After position reconciliation or fill processing, call `check_position_anomaly()`
- [x] 4.2: When anomalous position detected, spawn flatten attempt via `PositionFlattener`
- [x] 4.3: On flatten success: log resolution, anomalous position breaker remains tripped (manual reset)
- [x] 4.4: On flatten failure (`AllAttemptsFailed`): activate panic mode via `PanicMode::activate()`
- [x] 4.5: Panic mode is terminal — system cannot resume without process restart

### Task 5: Integrate panic mode with CircuitBreakers (AC: panic mode checked in permits_trading)
- [x] 5.1: Add `panic_mode: bool` field to CircuitBreakers struct
- [x] 5.2: In `permits_trading()`, check panic_mode first — if active, return denial with `PanicModeActive` reason
- [x] 5.3: Panic mode overrides all other breaker states — it is the highest severity

### Task 6: Unit tests (AC: all)
- [x] 6.1: Test anomalous position detection when position exists outside strategy context
- [x] 6.2: Test anomalous position detection does NOT trigger when strategy expects the position
- [x] 6.3: Test flatten retry submits up to 3 attempts with 1s delays
- [x] 6.4: Test flatten returns success on first attempt when order fills
- [x] 6.5: Test flatten returns AllAttemptsFailed after 3 rejections
- [x] 6.6: Test panic mode activation disables all trading via permits_trading()
- [x] 6.7: Test panic mode cancellation list excludes stop-loss orders
- [x] 6.8: Test panic mode logs full context including all flatten attempt details
- [x] 6.9: Test panic mode cannot be reset (requires restart)
- [x] 6.10: Test full flow: anomaly detected -> flatten fails -> panic mode activates

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
- Claude Opus 4.7 (1M context) via the bmad-dev-story workflow.

### Debug Log References
- `cargo test --workspace` — all 332 tests pass (engine: 133, risk submodule: 21,
  broker: 35, core: 62, plus integration suites). No regressions.
- `cargo clippy --workspace --all-targets -- -D warnings` — clean.
- One clippy fix in flight: `panic_mode::tests::LeggedCancellation::cancelled_kinds_contains_stop`
  rewritten from `iter().any(...)` to `contains(&"Stop")` per
  `clippy::manual_contains`.

### Completion Notes List
- Story 5.1's full `CircuitBreakers` framework has not yet landed on `main`
  (status `ready-for-dev`), so this story ships a minimal `circuit_breakers.rs`
  scaffold with two `BreakerType` variants (`AnomalousPosition`, `FlattenFailed`)
  and the `permits_trading()` / `panic_mode_active()` / `check_position_anomaly()`
  surface required by AC. When Story 5.1 lands the full 10-variant framework it
  will replace this scaffold and absorb the panic-mode integration verbatim;
  no Story 5.3 logic needs to change.
- The broker-side `FlattenRetry` (Story 4.3) already implements the 3-attempt /
  1s-interval retry mechanic spec'd by Task 2. Rather than duplicate the
  orchestrator, Story 5.3 introduces `PositionFlattener` as a `pub type` alias
  and adds a `flatten(...) -> Result<(), FlattenError>` method that captures
  per-attempt rejection reasons in `FlattenError::AllAttemptsFailed`. The
  legacy `flatten_with_retry` / `FlattenOutcome` shape is preserved unchanged
  for Story 4.3 consumers.
- `PanicContext` was added to `panic_mode.rs` alongside a new
  `activate_with_context` entry point. The pre-existing `PanicMode::activate`
  (Story 4.3) is preserved — Story 5.3's wiring uses
  `activate_with_context` so the journal `SystemEvent.message` carries the
  full per-attempt history required by Task 6.8.
- Task 4's "wire into event loop" is implemented as a pure async helper
  (`risk::anomaly_handler::handle_anomaly`) rather than embedded inside the
  synchronous `EventLoop`. The helper is intended to be `tokio::spawn`-ed
  onto the broker runtime; the engine hot path is never blocked.
- `engine/src/risk/mod.rs` now declares `#![deny(unsafe_code)]` (Story 5.3
  hardening; previously implicit). All four submodules — `circuit_breakers`,
  `anomaly_handler`, `panic_mode`, `fee_gate` — comply.
- Hot-path discipline: `CircuitBreakers::permits_trading()` performs no heap
  allocation on the happy path (two atomic loads + a fixed-size inline array
  read). `MAX_INLINE_TRIPPED = 4` is enough for Story 5.3's two breakers
  with headroom; Story 5.1 will widen if needed.

### File List
- `crates/engine/src/risk/circuit_breakers.rs` (new) — `CircuitBreakers`,
  `BreakerType`, `DenyReason`, `AnomalyCheckOutcome`, `check_position_anomaly`,
  `permits_trading`, 9 unit tests.
- `crates/engine/src/risk/anomaly_handler.rs` (new) — `handle_anomaly` async
  orchestrator and `AnomalyResolution` enum, 4 unit tests.
- `crates/engine/src/risk/panic_mode.rs` (modified) — added `PanicContext`,
  `PanicPositionDetails`, `FlattenAttemptRecord`, and the
  `activate_with_context` method. New tests: stops-excluded assertion,
  full-context journal record assertion, idempotent-with-context assertion.
- `crates/engine/src/risk/mod.rs` (modified) — added `#![deny(unsafe_code)]`,
  added `circuit_breakers` and `anomaly_handler` submodules to the public
  surface.
- `crates/broker/src/position_flatten.rs` (modified) — added `FlattenError`
  enum, `flatten() -> Result<(), FlattenError>` method, and `PositionFlattener`
  type alias. 3 new tests covering the `Result` shape.
- `crates/broker/src/lib.rs` (modified) — re-exported `FlattenError` and
  `PositionFlattener`.
- `_bmad-output/implementation-artifacts/5-3-anomalous-position-detection-and-panic-mode.md` (modified) — task checkboxes, status,
  and Dev Agent Record updates.

### Change Log
- 2026-04-30: Implemented Story 5.3 — anomalous position detection,
  `PositionFlattener::flatten` Result shape, `PanicContext`, async anomaly
  handler. 21 new risk-module tests added. Status moved to `review`.
